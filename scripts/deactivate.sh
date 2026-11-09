# Source this file to disable shims and restore the saved terminal environment.
if [ -n "${_APEXE_ACTIVE_SHIM_DIR-}" ]; then
  if [ -f "$_APEXE_ACTIVE_SHIM_DIR/deactivate" ]; then
    . "$_APEXE_ACTIVE_SHIM_DIR/deactivate"
  else
    # A manual --disable can remove the generated file before this shell restores.
    if [ "${_APEXE_PREVIOUS_PATH_SET+x}${_APEXE_PREVIOUS_PATH+x}${_APEXE_PREVIOUS_SHIM_DIR_SET+x}${_APEXE_PREVIOUS_SHIM_DIR+x}" != xxxx ]; then
      printf '%s\n' 'Saved activation environment is incomplete; refusing to disable' >&2
      return 1
    fi
    if ! command apexe env --disable --shim-dir "$_APEXE_ACTIVE_SHIM_DIR"; then return 1; fi
    if [ "$_APEXE_PREVIOUS_PATH_SET" = x ]; then
      export PATH="$_APEXE_PREVIOUS_PATH"
    else
      unset PATH
    fi
    if [ "$_APEXE_PREVIOUS_SHIM_DIR_SET" = x ]; then
      export APEXE_SHIM_DIR="$_APEXE_PREVIOUS_SHIM_DIR"
    else
      unset APEXE_SHIM_DIR
    fi
    unset _APEXE_ACTIVE_SHIM_DIR _APEXE_PREVIOUS_PATH_SET _APEXE_PREVIOUS_PATH
    unset _APEXE_PREVIOUS_SHIM_DIR_SET _APEXE_PREVIOUS_SHIM_DIR
  fi
elif [ -n "${APEXE_SHIM_DIR-}" ]; then
  command apexe env --disable --shim-dir "$APEXE_SHIM_DIR"
else
  # No directory is selected, so there is nothing this script can honestly
  # disable. It used to run `apexe env --disable` with the default directory,
  # which is whatever installation the *caller's* HOME happens to hold: a
  # sourced helper then takes down an installation it was never pointed at.
  # Name the directory instead of guessing.
  # Nothing is selected and nothing is active, so the shell already matches the
  # goal: succeed without touching any installation, and say so once.
  printf '%s\n' 'No apexe shim directory selected; nothing to deactivate' >&2
  return 0
fi
