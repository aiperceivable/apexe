# Source this file to leave the governed shell.
#
# Only this shell changes: the shim directory is removed from PATH and commands
# stop resolving through it, while entries added after activation stay where
# they are. Nothing on disk is removed -- uninstalling is
# `apexe env --disable`, a separate and deliberate action, because a helper
# named `deactivate` that takes the whole installation down is one keystroke
# away from being run by accident.
if [ -n "${_APEXE_ACTIVE_SHIM_DIR-}" ]; then
  if [ -f "$_APEXE_ACTIVE_SHIM_DIR/deactivate" ]; then
    . "$_APEXE_ACTIVE_SHIM_DIR/deactivate"
  else
    # The generated file is gone (a manual --disable removes it), so leave the
    # governed shell from the environment the activation saved. Restoring the
    # saved PATH wholesale dropped entries added after activation, so only the
    # shim directory is filtered out of the current PATH.
    if [ "${_APEXE_PREVIOUS_PATH_SET+x}${_APEXE_PREVIOUS_PATH+x}${_APEXE_PREVIOUS_SHIM_DIR_SET+x}${_APEXE_PREVIOUS_SHIM_DIR+x}" != xxxx ]; then
      printf '%s\n' 'Saved activation environment is incomplete; refusing to leave the governed shell' >&2
      return 1
    fi
    if [ "$_APEXE_PREVIOUS_PATH_SET" = x ]; then
      _APEXE_REMAINING="${PATH-}:"
      _APEXE_FILTERED=
      _APEXE_SEPARATOR=
      while [ -n "$_APEXE_REMAINING" ]; do
        _APEXE_ENTRY="${_APEXE_REMAINING%%:*}"
        _APEXE_REMAINING="${_APEXE_REMAINING#*:}"
        if [ "$_APEXE_ENTRY" = "$_APEXE_ACTIVE_SHIM_DIR" ]; then
          continue
        fi
        _APEXE_FILTERED="${_APEXE_FILTERED}${_APEXE_SEPARATOR}${_APEXE_ENTRY}"
        _APEXE_SEPARATOR=:
      done
      PATH="$_APEXE_FILTERED"
      export PATH
      unset _APEXE_REMAINING _APEXE_FILTERED _APEXE_SEPARATOR _APEXE_ENTRY
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
    printf '%s\n' 'This shell no longer resolves shims. The installation is unchanged; remove it with apexe env --disable' >&2
  fi
elif [ -n "${APEXE_SHIM_DIR-}" ]; then
  # A directory is named but this shell never saved the environment it replaced,
  # so there is nothing to restore. Say so instead of uninstalling the directory.
  printf '%s\n' "This shell has no saved activation for $APEXE_SHIM_DIR; remove that directory from PATH yourself, or uninstall it with apexe env --disable --shim-dir $APEXE_SHIM_DIR" >&2
  return 0
else
  # Nothing is selected and nothing is active, so the shell already matches the
  # goal: succeed without touching any installation, and say so once.
  printf '%s\n' 'No apexe shim directory selected; nothing to deactivate' >&2
  return 0
fi
