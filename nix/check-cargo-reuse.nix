{ pkgs }:
pkgs.runCommand "check-cargo-reuse" { nativeBuildInputs = [ pkgs.jq ]; } ''
  cat > metadata.json <<'JSON'
  {"workspace_members": ["path+file:///build/picky#0.1.0"]}
  JSON

  check_log() {
    jq --exit-status --slurp --slurpfile metadata metadata.json \
      --from-file ${./check-cargo-reuse.jq} "$1"
  }

  # Workspace compilation and non-artifact messages are allowed; ordinary
  # dependencies, proc macros, and build scripts must all be reused.
  cat > reused.json <<'JSON'
  {"reason":"compiler-artifact","package_id":"path+file:///build/picky#0.1.0","fresh":false}
  {"reason":"compiler-artifact","package_id":"registry+example#ordinary","fresh":true}
  {"reason":"compiler-artifact","package_id":"registry+example#proc-macro","fresh":true}
  {"reason":"compiler-artifact","package_id":"registry+example#build-script","fresh":true}
  {"reason":"build-script-executed","package_id":"registry+example#build-script"}
  {"reason":"build-finished","success":true}
  JSON
  check_log reused.json

  for crate in ordinary proc-macro build-script; do
    cp reused.json rebuilt.json
    jq -nc --arg id "registry+example#$crate" \
      '{reason:"compiler-artifact", package_id:$id, fresh:false}' >> rebuilt.json
    if check_log rebuilt.json > rejected.log 2>&1; then
      echo "Guard incorrectly accepted a rebuilt $crate" >&2
      exit 1
    fi
    grep -F "Dependencies recompiled in the application stage: registry+example#$crate" rejected.log
  done

  touch "$out"
''
