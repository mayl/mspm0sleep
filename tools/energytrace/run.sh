#!/usr/bin/env bash
# Launch CCS Theia with a project-local workspace for EnergyTrace capture.
#
# TI does not ship a CLI for EnergyTrace on MSPM0 targets. This wrapper
# just opens the IDE with a workspace under tools/energytrace/ so the
# .ccxml target config and exported CSVs land in a predictable place.
set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
WORKSPACE="$REPO_ROOT/tools/energytrace/workspace"
CAPTURES="$REPO_ROOT/tools/energytrace/captures"

mkdir -p "$WORKSPACE" "$CAPTURES"

if command -v ccs-theia >/dev/null 2>&1; then
  CCS_THEIA=(ccs-theia)
else
  CCS_THEIA=(nix run "$REPO_ROOT#ccs-theia" --)
fi

cat <<EOF
─────────────────────────────────────────────────────────────────
 CCS Theia — EnergyTrace capture
─────────────────────────────────────────────────────────────────
 Workspace: $WORKSPACE
 Save captures under: $CAPTURES

 First run inside the IDE:
   1. File → New → Target Configuration File
        Connection: Texas Instruments XDS110 USB Debug Probe
        Board/Device: MSPM0L1306
        Save as mspm0l1306.ccxml in the workspace.
   2. Right-click the .ccxml → Launch Selected Configuration.
   3. View → Other → EnergyTrace.
   4. Connect target, press the red record button.
   5. Stop, then Save (CSV) under $CAPTURES.

 Plug the LaunchPad in (XDS110 USB) before connecting in the IDE.
─────────────────────────────────────────────────────────────────
EOF

exec "${CCS_THEIA[@]}" "$WORKSPACE" "$@"
