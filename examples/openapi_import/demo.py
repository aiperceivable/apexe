"""Import a local HTTP API through MCP/A2A and verify ACL and credential separation."""
import argparse
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import secrets
import selectors
import shutil
import socket
import subprocess
import tempfile
import threading
import time
from urllib.parse import parse_qs, unquote, urlsplit
from urllib.request import ProxyHandler, Request, build_opener


HTTP_OPENER = build_opener(ProxyHandler({}))


@contextmanager
def api_server(specification):
    api_token, spec_token = secrets.token_urlsafe(24), secrets.token_urlsafe(24)
    calls, spec_fetches = [], []

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_arguments):
            pass

        def reply(self, status, body):
            data = json.dumps(body).encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def authorized(self):
            if self.headers.get("Authorization") != f"Bearer {api_token}":
                self.reply(401, {"error":"invalid API credential"})
                return False
            if self.headers.get("X-Spec-Key") is not None:
                self.reply(400, {"error":"specification credential reached the API"})
                return False
            return True

        def do_GET(self):
            parsed = urlsplit(self.path)
            if parsed.path == "/openapi.json":
                if self.headers.get("X-Spec-Key") != spec_token:
                    return self.reply(401, {"error":"invalid specification credential"})
                spec_fetches.append(True)
                return self.reply(200, specification)
            if self.authorized() and parsed.path.startswith("/pets/"):
                calls.append(("GET", parsed.path))
                self.reply(200, {"id":unquote(parsed.path[6:]), "name":"demo",
                                 "query":parse_qs(parsed.query).get("q", [None])[0]})

        def do_POST(self):
            if not self.authorized():
                return
            data = json.loads(self.rfile.read(int(self.headers.get("Content-Length", "0"))))
            calls.append(("POST", self.path))
            self.reply(201, {"id":str(len(calls)), "name":data["name"]})

        def do_DELETE(self):
            if self.authorized():
                calls.append(("DELETE", self.path))
                self.reply(200, {"deleted":True})

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield {"url":f"http://127.0.0.1:{server.server_port}", "api_token":api_token,
               "spec_token":spec_token, "calls":calls, "spec_fetches":spec_fetches}
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)


class McpSession:
    def __init__(self, command, environment, directory):
        self.errors = tempfile.TemporaryFile()
        self.process = subprocess.Popen(command, env=environment, cwd=directory,
                                        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.errors, bufsize=0)
        self.sequence = 0

    def __enter__(self):
        try:
            self.request("initialize", {"protocolVersion":"2024-11-05", "capabilities":{},
                         "clientInfo":{"name":"openapi-example", "version":"1"}})
            self.send({"jsonrpc":"2.0", "method":"notifications/initialized"})
            return self
        except Exception:
            self.close()
            raise

    def __exit__(self, *_arguments):
        self.close()

    def close(self):
        self.process.terminate()
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait(timeout=5)
        self.process.stdin.close()
        self.process.stdout.close()
        self.errors.close()

    def send(self, payload):
        self.process.stdin.write((json.dumps(payload) + "\n").encode())
        self.process.stdin.flush()

    def request(self, method, parameters):
        self.sequence += 1
        self.send({"jsonrpc":"2.0", "id":self.sequence, "method":method, "params":parameters})
        deadline = time.monotonic() + 10
        with selectors.DefaultSelector() as selector:
            selector.register(self.process.stdout, selectors.EVENT_READ)
            while time.monotonic() < deadline:
                if not selector.select(max(0, deadline - time.monotonic())):
                    break
                line = self.process.stdout.readline()
                if not line:
                    raise RuntimeError("MCP server exited before responding")
                response = json.loads(line)
                if response.get("id") == self.sequence:
                    return response
        raise TimeoutError(f"MCP {method} did not respond")


def mcp_demo(command, environment, work, api, shared):
    with McpSession([*command, "serve", *shared, "--transport", "stdio", "--no-log-arguments"], environment, work) as session:
        tools = session.request("tools/list", {})["result"]["tools"]
        names = {operation:next(tool["name"] for tool in tools if operation in tool["name"])
                 for operation in ["get_pet", "create_pet", "delete_pet"]}
        read = session.request("tools/call", {"name":names["get_pet"], "arguments":{"id":"42", "q":"from-mcp"}})
        created = session.request("tools/call", {"name":names["create_pet"], "arguments":{"name":"example-mcp"}})
        for response, expected in [(read, "from-mcp"), (created, "example-mcp")]:
            if "error" in response or response.get("result", {}).get("isError") or expected not in json.dumps(response):
                raise RuntimeError("Imported MCP operation failed")
        count = len(api["calls"])
        denied = session.request("tools/call", {"name":names["delete_pet"], "arguments":{"id":"42"}})
        if not ("error" in denied or denied.get("result", {}).get("isError")) or len(api["calls"]) != count:
            raise RuntimeError("ACL denial must stop the upstream DELETE request")
        return {"read":True, "json_body":True, "delete_denied":True}


def free_port():
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def a2a_request(url, token, skill, arguments):
    payload = {"jsonrpc":"2.0", "id":1, "method":"message/send", "params":{"message":{
        "messageId":secrets.token_hex(8), "role":"ROLE_USER", "parts":[{"data":arguments}], "metadata":{"skillId":skill}}}}
    request = Request(url, data=json.dumps(payload).encode(), headers={"Content-Type":"application/json", "Authorization":f"Bearer {token}"})
    with HTTP_OPENER.open(request, timeout=10) as response:
        return json.load(response)


def a2a_demo(command, environment, work, api, shared):
    url = f"http://127.0.0.1:{free_port()}"
    with tempfile.TemporaryFile() as errors:
        process = subprocess.Popen([*command, "a2a", *shared, "--url", url, "--no-log-arguments"], env=environment,
                                   cwd=work, stdout=subprocess.DEVNULL, stderr=errors)
        try:
            wait_for_health(url, process)
            for skill, arguments in [("api.get_pet", {"id":"42", "q":"from-a2a"}),
                                     ("api.create_pet", {"name":"example-a2a"})]:
                response = a2a_request(url, environment["APEXE_AUTH_TOKEN"], skill, arguments)
                if response.get("result", {}).get("status", {}).get("state") != "TASK_STATE_COMPLETED":
                    raise RuntimeError("Imported A2A operation failed")
            count = len(api["calls"])
            denied = a2a_request(url, environment["APEXE_AUTH_TOKEN"], "api.delete_pet", {"id":"42"})
            if denied.get("result", {}).get("status", {}).get("state") != "TASK_STATE_REJECTED" or len(api["calls"]) != count:
                raise RuntimeError("A2A ACL denial must stop the upstream DELETE request")
            return {"authenticated":True, "read":True, "json_body":True, "delete_denied":True}
        finally:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)


def wait_for_health(url, process):
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError("A2A example server exited during startup")
        try:
            with HTTP_OPENER.open(url + "/health", timeout=0.25) as response:
                if response.status == 200:
                    return
        except OSError:
            time.sleep(0.05)
    raise TimeoutError("A2A example server did not become healthy")


def run(apexe, surface, remote):
    directory = Path(__file__).resolve().parent
    specification = json.loads((directory / "openapi.json").read_text(encoding="utf-8"))
    with tempfile.TemporaryDirectory(prefix="apexe-openapi-example-") as temporary, api_server(specification) as api:
        root = Path(temporary)
        environment = {"HOME":str(root), "PATH":os.defpath, "RUST_LOG":"error", "LC_ALL":"C",
                       "APEXE_OPENAPI_TOKEN":api["api_token"], "APEXE_AUTH_TOKEN":secrets.token_urlsafe(24)}
        source = api["url"] + "/openapi.json" if remote else str(directory / "openapi.json")
        shared = ["--openapi", source, "--openapi-base-url", api["url"], "--acl", str(directory / "acl.yaml")]
        if remote:
            shared.extend(["--openapi-header", f"X-Spec-Key: {api['spec_token']}"])
        # Shared flags are subcommand arguments, not global apexe options.
        results = {}
        for mode in (["mcp", "a2a"] if surface == "both" else [surface]):
            command = [apexe]
            runner = mcp_demo if mode == "mcp" else a2a_demo
            results[mode] = runner(command, environment, root, api, shared)
        if any(method == "DELETE" for method, _path in api["calls"]):
            raise RuntimeError("A denied operation reached the API")
        return {"surfaces":results, "upstream_calls":len(api["calls"]), "spec_fetches":len(api["spec_fetches"]),
                "spec_credentials_not_forwarded":True, "temporary_home_removed":True}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--apexe", default="apexe")
    parser.add_argument("--surface", choices=["mcp", "a2a", "both"], default="both")
    parser.add_argument("--remote-spec", action="store_true", help="Fetch the spec with separate credentials")
    arguments = parser.parse_args()
    executable = shutil.which(arguments.apexe)
    if not executable:
        parser.error("apexe executable was not found; build it or supply --apexe")
    print(json.dumps(run(str(Path(executable).resolve()), arguments.surface, arguments.remote_spec), indent=2))


if __name__ == "__main__":
    main()
