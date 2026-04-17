# Deferred Prompts

* performance issues. What's taking so long (where is the overhead coming from)? Do a full optimization pass. Don't bother making the AI good. Even remove it if it is a lot of work right now. Use caching where possible. find better algorithms.
* make a dedicated UI pass: the UI should resemble the original as closely as possible.
* any bevy performance improvements we can make?
* investigate the post-processing pipeline of this game vs. spring/kp.
* double-click to select same units
* unit groups
* find which performance tweaks spring gets away with. Do they apply to us?
* fix the skybox. Should be same as original kernel-panic.
* start going on the web-assets-plan.md.
* implement the game UI
* investigate how to do server-authoritative multiplayer using matchbox and lightyear.
* fix unit placement when using builders, just like the original
* use a walk dir crate. Find the best one first.
* don't use std::process::exit, use error handling, returning from main.
* remove map cycling
* load_current_map and load_map_at_index are redundant
* find python-style super-terse variable names. We don't do this here, make them speak.
* there should not be any fov. The entire map is always visible. Just not any buildings or units that were built.
* use cargo-flamegraph. Do you find anything interesting?
* large enum variants, especially with annotations and doc comments, should have a newline before the next variant starts. Everywhere. This also goes for structs!
* ALL_UNIT_KINDS should be derived
* same for SHOWCASE_KINDS and others like it
* make another pass identifying special numerical or string values. Make them enums.
* simplify pub fn load_asset_from_disk<T, E: fmt::Display>.
* In general, find functions or methods which get passed a function. It should normally be possible and lead to broad design simplifications.
* find places where error handling is skipped and log them.
* find functions returning options which internally map a result to an option. Return the result instead and let the caller make the decision.
* Make a pass over the tests. Run cargo llvm-cov and find gaps which would be useful to cover.
* maybe reduce usage of println and eprintln, use logging instead.
* Inspect the lib crates of this workspace. What is their public api? Can it be improved? Can it become more idiomatic?
* any way you can reduce the number of arguments for the bevy system functions? It seems excessive in parts.
* what about glyph_zero and glyph_one? would this be more optimal with an actual asset, or is it fastest like this?
* ORDERS should be more idiomatic - it has 2 str's and an item, should be enums. Should be a struct in total, maybe?
* Find colors, make them named constants. Name them by purpose.
* The Interaction enum has a completely wild default implementation. Derive it instead, and look that this pattern is not used anywhere else either, like in FocusPolicy.
