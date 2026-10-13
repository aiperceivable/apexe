# Source this file in sh, bash or zsh to enable shims and activate this terminal.
# Reload retains installed settings and performs enable when no manifest exists.
if [ -n "${APEXE_SHIM_DIR-}" ]; then
  if ! command apexe env --reload --shim-dir "$APEXE_SHIM_DIR"; then return 1; fi
  _APEXE_ACTIVATION_OUTPUT="$(command apexe env --shim-dir "$APEXE_SHIM_DIR" --shell sh)" || { unset _APEXE_ACTIVATION_OUTPUT; return 1; }
else
  if ! command apexe env --reload; then return 1; fi
  _APEXE_ACTIVATION_OUTPUT="$(command apexe env --shell sh)" || { unset _APEXE_ACTIVATION_OUTPUT; return 1; }
fi
if ! eval "$_APEXE_ACTIVATION_OUTPUT"; then unset _APEXE_ACTIVATION_OUTPUT; return 1; fi
unset _APEXE_ACTIVATION_OUTPUT
