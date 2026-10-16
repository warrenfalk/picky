# Workspace crates must compile; every external dependency must be fresh.
# Cargo's JSON output includes build scripts and proc macros as artifacts too.
[
  .[]
  | select(.reason == "compiler-artifact" and .fresh != true)
  | select(.package_id as $id | $metadata[0].workspace_members | index($id) | not)
  | .package_id
]
| unique
| if length == 0 then
    "All dependency artifacts were reused"
  else
    error("Dependencies recompiled in the application stage: \(join(", ")). Keep both Crane stages' settings aligned; see BUILDING.md")
  end
