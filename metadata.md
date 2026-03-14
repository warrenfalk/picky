# Relm4 Best Practices Metadata

This file contains metadata for `relm4-best-practices.md`: version baseline, confidence notes, and the research sources used to build the guide.

## Version baseline

- Target baseline: Relm4 `0.10.1`
- Current source of truth: Relm4 API docs and upstream changelog
- Caveat: the Relm4 book still contains some older version references, so architectural guidance is more stable than exact API spelling

## Confidence notes

- High confidence: current API docs, current README, changelog, official examples, and book chapters whose advice is architectural rather than version-specific
- Medium confidence: blog posts explaining the intent behind architectural changes that still exist in the current API surface
- Lower confidence: older Relm material and community discussions, useful mainly for rationale and recurring failure modes

## Application sample caveat

- `done` currently targets `relm4 = "0.7.0-beta.2"`
- `fm` currently targets `relm4 = "0.9.0"`
- `nixos-conf-editor` currently targets `relm4 = "0.5.1"`

These repos are still valuable for architecture, but any exact API details from them must be reconciled against current docs before being treated as guidance.

## Sources

Official Relm4 sources used for the guide:

- <https://github.com/Relm4/Relm4>
- <https://github.com/Relm4/Relm4/blob/main/CHANGES.md>
- <https://relm4.org/docs/stable/relm4/>
- <https://relm4.org/book/stable/>
- <https://relm4.org/book/stable/first_app.html>
- <https://relm4.org/book/stable/component_macro.html>
- <https://relm4.org/book/stable/components.html>
- <https://relm4.org/book/next/tricks.html>
- <https://relm4.org/book/stable/efficient_ui/factory.html>
- <https://relm4.org/book/stable/efficient_ui/tracker.html>
- <https://relm4.org/book/stable/widget_templates/index.html>
- <https://relm4.org/docs/next/relm4/shared_state/index.html>
- <https://relm4.org/docs/next/relm4_macros/macro.view.html>
- <https://relm4.org/blog/posts/announcing_relm4_v0.5_beta/>
- <https://relm4.org/blog/posts/announcing_relm4_v0.5/>
- <https://relm4.org/blog/posts/announcing_relm4_v0.7/>
- <https://relm4.org/blog/posts/gui_speedrun/>

Official code used for validation:

- <https://github.com/Relm4/Relm4/blob/main/examples/simple.rs>
- <https://github.com/Relm4/Relm4/blob/main/examples/simple_manual.rs>
- <https://github.com/Relm4/Relm4/blob/main/examples/components.rs>
- <https://github.com/Relm4/Relm4/blob/main/examples/worker.rs>
- <https://github.com/Relm4/Relm4/blob/main/examples/non_blocking_async.rs>
- <https://github.com/Relm4/Relm4/blob/main/examples/simple_async.rs>
- <https://github.com/Relm4/Relm4/blob/main/examples/factory.rs>
- <https://github.com/Relm4/Relm4/blob/main/examples/tracker.rs>
- <https://github.com/Relm4/Relm4/blob/main/examples/widget_template.rs>
- <https://github.com/Relm4/Relm4/blob/main/examples/tab_game.rs>
- <https://github.com/Relm4/Relm4/blob/main/examples/state_management.rs>

Application-scale repos sampled for pattern validation:

- <https://github.com/edfloreshz/done>
- <https://github.com/euclio/fm>
- <https://github.com/vlinkz/nixos-conf-editor>

Historical Relm rationale consulted for transferable anti-patterns:

- <https://relm.antoyo.xyz/relm-release-0.10/>
- <https://relm.antoyo.xyz/big-release/>

Selected community threads used for specific gotchas:

- <https://github.com/orgs/Relm4/discussions/552>
- <https://github.com/orgs/Relm4/discussions/617>
- <https://github.com/orgs/Relm4/discussions/496>
