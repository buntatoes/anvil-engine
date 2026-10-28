<div align="center">
  <h1>Anvil Engine</h1>
  <p>Page pipeline for Anvil Browser. Not the window, the network stack, or the profile file.</p>
  <p>
    <img src="https://img.shields.io/badge/release-v.23.3.2-ff6b24?style=flat-square" alt="v.23.3.2" />
    <img src="https://img.shields.io/badge/status-Proof_of_Concept-d4a359?style=flat-square" alt="Proof of Concept" />
    <img src="https://img.shields.io/badge/clean_code-clippy_%2B_rustfmt_%280_warnings%29-2ea44f?style=flat-square" alt="Clean code check: clippy and rustfmt passing with 0 warnings" />
    <img src="https://img.shields.io/badge/external_crates-0_%28offline_verified%29-2ea44f?style=flat-square" alt="0 external crates (offline verified)" />
    <a href="./LICENSE"><img src="https://img.shields.io/badge/license-PolyForm_Noncommercial_1.0.0_%7C_Paid_Commercial_%28Non--Compete%29-blue?style=flat-square" alt="PolyForm Noncommercial 1.0.0 | Paid Commercial (Non-Compete)" /></a>
    <img src="https://img.shields.io/badge/rust-2024-orange?style=flat-square" alt="Rust 2024" />
  </p>
</div>

Version 23.3.2 (`v.23.3.2`). Author: Buntato.

This repository is part of the [Anvil Browser](https://github.com/buntatoes/anvil-browser) ecosystem and is dual-licensed under the **PolyForm Noncommercial License 1.0.0** and a **Paid Commercial License with a Non-Compete Clause**. See [License](#license).

There are no crates.io dependencies.

## Pipeline

| Crate | What it is |
| --- | --- |
| `anvil-dom` | HTML parser and DOM tree: `<noscript>` isolation, sibling/child traversal, `get_elements_by_tag_name` / `get_elements_by_class_name` / `get_elements_by_name`, `#fragment` anchor lookup (`find_fragment_target`), `<details>`/`<summary>` disclosure helpers, `<input type="radio">` groups, `<label>` control resolution, per-control text caret and word-deletion helpers, `<select>` option helpers, `<input type="range">` / `<progress>` / `<meter>` state, Shadow DOM, Custom Elements, `<canvas>` 2D RGBA backing surfaces (`CanvasSurface`), and Selectors Level 4 matching (`:is()`, `:where()`, `:not()`, `:has()`, `:host`, `:defined`) |
| `anvil-style` | CSS parsing (`anvil-css` 0.3.0), Selectors Level 4, CSS Color Level 4, `@media` and `@supports` queries, custom properties (`--var`), `calc()`, `box-sizing: border-box`, `aspect-ratio`, `visibility`, `text-align`, `white-space`, `line-height`, `inset`, `flex` and `grid` shorthands, `margin: 0 auto`, closed `<dialog>` and `<details>` body suppression, keyframe animations, and parallel style resolution (`compute_styles_parallel`) |
| `anvil-layout` | Block, inline, `inline-block`, flex (`row` and `column`), grid (`minmax()`, `auto-fill` / `auto-fit`), table (`colspan`), and positioned layout: `aspect-ratio` sizing, multi-line `<pre>` and `<textarea>` formatting, mid-string caret (`|`) rendering, track/fill/thumb bitmaps for `<input type="range">`, `<progress>`, and `<meter>`, `<summary>` and `<input type="radio">` indicators, inline `<svg>` vector icon and path rasterizer, `<canvas>` surface blitting, hit testing, and range text selection (`collect_text_runs_in_rect`) |
| `anvil-paint` | Display list builder (honoring `visibility: hidden`, clips, transforms, and fixed/sticky layers) and page zoom |
| `anvil-raster` | RGBA framebuffer (with fast row-slice blitting `copy_rows_from`), batched `anvil_gpu::Compositor` scene execution (`GpuSceneBatch`), bounded LRU glyph coverage cache, and multi-core tile rasterization (`RasterEngine`) |
| `anvil-script` | Budgeted JavaScript and WebAssembly 1.0 runtime: Rust interpreter, WebAssembly stack VM (`WasmModule`, `WasmInstance`, `WasmMemory`), numeric x86-64 loop compiler, and `anvil-js` 0.5.0 ES2022+ compiler and DOM runtime on `AnvilVM` with live DOM/storage/location sync, interactive `click`/`input`/`change`/`submit`/`keydown` event dispatch, microtask-queued `MutationObserver` / `IntersectionObserver` / `ResizeObserver` callbacks, 2D `<canvas>` context, and WebCodecs/MSE helpers |

`anvil-script` does not open a socket. `fetch()` is performed by the host in `anvil-browser`.

## Scripts and WebAssembly 1.0 (`WASM`)

`anvil-script` runs timers, promises, WebAssembly 1.0 modules, and DOM mutations on its own event loop. After each task the whole microtask queue is drained. Timers fire by due time, then in the order they were set, on a virtual clock that does not follow wall time. A timer set while a timer callback itself runs waits at least 4 ms. Timers set from the microtasks that callback queues, or after an `await` in an async timer callback, keep the delay they ask for.

Supported:

- WebAssembly 1.0 (`\0asm` version `1`) binary module decoding (`WasmModule::decode`), validation (`validate_wasm`), bounded 64 KiB page linear memory (`WasmMemory`, up to 256 pages / 16 MiB), and stack machine execution (`WasmInstance::instantiate`, `call_export`, `get_exported_global`, `set_exported_global`) covering `i32`, `i64`, `f32`, and `f64` arithmetic, bitwise, comparison, conversion, load/store, `memory.size`, `memory.grow`, `memory.copy`, `memory.fill`, and structured control flow (`block`, `loop`, `if`/`else`, `br`, `br_if`, `br_table`, `call`, `call_indirect`, `return`)
- JavaScript `WebAssembly` API: `WebAssembly.validate`, `WebAssembly.compile`, `WebAssembly.compileStreaming`, `WebAssembly.instantiate`, `WebAssembly.instantiateStreaming`, `new WebAssembly.Module`, `new WebAssembly.Instance`, `new WebAssembly.Memory`, `new WebAssembly.Global`, `new WebAssembly.Table`, calling exported WASM functions (`instance.exports.<fn>(...)`), and `memory.grow(delta)`
- `TypedArray` and binary web APIs: `Uint8Array`, `Uint8ClampedArray`, `Int8Array`, `Uint16Array`, `Int16Array`, `Uint32Array`, `Int32Array`, `Float32Array`, `Float64Array` (including views over `WasmMemory` buffers), `ArrayBuffer`, `TextEncoder`, `TextDecoder`, `atob`, `btoa`, `structuredClone`, `URLSearchParams`, `DOMParser`, `AbortController`, `Event`, and `CustomEvent`
- Interactive hydration (`ScriptRuntime::hydrate_interactive_bindings`) for documents returned from `--tab-worker`, preserving `onclick`/`oninput`/`onchange`/`onsubmit`/`onkeydown` attributes, `addEventListener('click' | 'input' | 'change' | 'submit' | 'keydown', ...)` listeners, ES2022+ `AnvilVM` interactive scripts, and declared functions/classes in the browser window without re-running initial DOM mutations
- Microtask-queued `MutationObserver` (`observe` with `childList`, `attributes`, `characterData`, `subtree`, `disconnect`, `takeRecords`), `IntersectionObserver` (`observe`, `unobserve`, `disconnect`, `takeRecords`), and `ResizeObserver` (`observe`, `unobserve`, `disconnect`) callbacks; live `dispatch_click`, `dispatch_input`, `dispatch_submit`, and `dispatch_keydown` with `event.preventDefault()` (`last_event_default_prevented()`)
- `new Promise`, `then`, `catch`, `finally`, and `Promise.resolve`, `reject`, `all`, `allSettled`, `race`, `any`
- A promise resolved with another promise follows it. A class instance with a `then` method is called like one
- A rejection with no handler is counted (`unhandled_rejections()`) and does not stop the page
- `setTimeout`, `setInterval`, `clearTimeout`, `clearInterval`, `queueMicrotask`. `requestAnimationFrame` runs as a timer
- `async function`, async arrows (also called in place, as in `(async () => { ... })()`), async callbacks passed to `then`, timers, and `queueMicrotask`
- `await` suspends the async function. It resumes from a microtask, so code after the call runs first. This works inside `if`, `while`, `for`, `for...of`, blocks, and `try`/`catch`/`finally`
- `for await...of` over an array of values and promises
- Overlapping calls of one async function keep their own parameters and locals
- An async call that is not awaited, from the script or from inside another async function, still runs to its end

Not supported, or different:

- Async generators, and `for await...of` over anything but an array
- Objects with a `then` property that are not class instances are plain values
- An `await` in a `switch`, a `do...while`, a loop condition, or at the top level of a script waits in place. It runs the event loop until the promise settles instead of yielding
- `async` class methods return a promise, but their awaits wait in place
- A callback created inside an async function gets a copy of that function's locals, not a live link
- No `unhandledrejection` event
- `IntersectionObserver` and `ResizeObserver` deliver initial microtask entry records when `observe(el)` is called; they do not re-fire continuously during viewport scroll or window resize

Limits: a turn spends at most 200,000 steps (`MAX_EXECUTION_STEPS`), script source and `fetch()` bodies are capped at 256 KiB, and WebAssembly calls are bounded at 500,000 instructions (`MAX_WASM_STEPS`), 64 call frames (`MAX_WASM_CALL_DEPTH`), and 256 linear-memory pages (16 MiB). Every microtask, timer callback, and resumed `await` costs steps, so a promise flood, a timer that sets itself again, or an endless `setInterval` is stopped with a reason (`abort_reason()`) instead of hanging. At most 128 timers and 8,192 queued microtasks are kept at once. ES2022+ DOM scripts (using `#private` fields, `static {}` blocks, logical assignment `??=`/`||=`/`&&=`, or `"use anvil-vm"`) execute on `AnvilVM` through the hierarchical Live DOM Host Bridge; other scripts that use the event loop, DOM, or `WebAssembly` run on the Rust interpreter.

## CSS support

The crate docs list every property. These are the newer parts and their limits.

### Parsing and the cascade

- Order: user-agent defaults, then author rules by specificity and source order, then the `style` attribute, then `!important` author rules, then `!important` in the `style` attribute.
- Strings, backslash escapes, and the body of an unquoted `url(...)` are skipped when looking for `{`, `}`, and `;`, so a brace or semicolon inside them doesn't break the sheet.
- `@media` reads `min-width`, `max-width`, and `prefers-color-scheme` (only `dark` matches). The page is a `screen`, so `all` and `screen` match and `print` doesn't. `not`, `only`, `and`, and comma lists work, and nested `@media` is combined with the outer one.
- `Document::style_sheets` lists every `<link>` and `<style>` in document order with its `media` attribute. `parse_css_with_media` parses one sheet under that media, and `Stylesheet::append` puts sheets together in order.

### Selectors

- Supported: all four combinators; attribute selectors with every operator and the `i` and `s` flags; `:is()`, `:where()`, `:not()`, and `:has()`; `:host`, `:host()`, and `:host-context()` (on shadow host elements); `:nth-child()`, `:nth-last-child()`, `:nth-of-type()`, and `:nth-last-of-type()`, with `of S` on the child ones; the structural pseudo-classes (`:root`, `:empty`, `:first-child`, and the rest); `:link` and `:any-link`; the form states (`:checked`, `:default`, `:disabled`, `:enabled`, `:required`, `:optional`, `:read-only`, `:read-write`, `:placeholder-shown`, `:indeterminate` on `progress`); `:open`, `:closed`, `:defined`, `:lang()`, and `:dir()`; and CSS escapes. Specificity follows the selector spec.
- Valid but never match: standard and vendor-prefixed (`-vendor-*`, `-moz-*`, `-ms-*`) pseudo-elements and pseudo-classes; namespace prefixes other than `*|`, since `@namespace` isn't read; and states a static page never has, such as `:hover`, `:active`, `:focus`, `:focus-visible`, `:focus-within`, `:visited`, `:target`, `:valid`, `:invalid`, `:user-valid`, `:user-invalid`, `:in-range`, `:out-of-range`, `:autofill`, `:fullscreen`, `:modal`, `:popover-open`, the media playback states, and `:state()`. The full list is on `Selector`. `:indeterminate` on radio buttons doesn't match either, and `:dir(auto)` reads as left to right.
- Not valid: `&` (nesting). A selector nested more than 16 deep (`MAX_SELECTOR_NESTING`) or longer than 64 compounds (`MAX_SELECTOR_COMPOUNDS`) drops its list.
- `Document::query_selector` and `Document::query_selector_all` support combinators, attribute selectors, `:is()`, `:where()`, `:not()`, `:has()`, `:host`, `:host()`, `:host-context()`, and `:defined`.

### Gaps and sizes

- `row-gap`, `column-gap`, `gap`, and the old `grid-` names take lengths (including `ch`), percentages, `calc()`, and `normal`. Grid uses them between tracks. Flex rows use `column-gap` between items and `row-gap` between lines; flex columns use `row-gap`. A `row-gap` percentage counts as 0 when the container's height isn't set.
- A set `height` is the height. Content that doesn't fit overflows, and is clipped when `overflow` is `hidden`, `auto`, or `scroll`.
- Attributes on the `html`, `head`, and `body` tags are kept, so `<body style>`, `<body class>`, and `<html lang>` work.

### Grid

- Supported: `grid-template-columns` and `grid-template-rows` with `px`, `%`, `fr`, `auto`, `minmax()`, and `repeat()` (including `repeat(auto-fill, ...)` and `repeat(auto-fit, ...)` on column tracks); `grid-auto-rows`; `grid-column`, `grid-row`, their `-start` / `-end` longhands, and numeric `grid-area`, with line numbers, negative lines, and `span N`; sparse row-by-row auto-placement that flows around spanning items; `align-items: center` and `end` inside grid areas.
- Not supported: named lines, `grid-template-areas` and named `grid-area`, `grid-auto-flow`, `grid-auto-columns`, `subgrid`, and implicit tracks before line 1 (such an item starts at line 1). Lines past 100 are clamped. An `auto` column acts as `1fr`.

### `ch`

- Supported on `width`, `height`, `max-width`, and the offsets. One `ch` is the advance of `0` in the system font when `anvil-ttf` loads one (Linux and Windows); otherwise it is half the font size, the advance layout uses for every character.
- Also supported on `row-gap`, `column-gap`, and `gap`.
- Not supported: `ch` in margin, padding, border, `font-size`, or `calc()`.

### `position: sticky`

- Supported: `top`, `right`, `bottom`, and `left` (`px`, `%`, and the other length units) against the viewport or the nearest ancestor with hidden overflow, bounded by the parent's content box. Viewport stickiness is applied at draw time for the scroll position, in every raster path and at every zoom; `hit_test_scrolled` finds boxes where they are drawn. `top` wins over `bottom` and `left` over `right`.
- Not supported: scrolling overflow containers (`overflow: auto` and `scroll` clip but do not scroll, so a sticky box inside one is placed once) and horizontal page scrolling.

### Transforms

- Supported: `transform` with up to 8 of `translate()`, `translateX()`, `translateY()`, `scale()`, `scaleX()`, `scaleY()`, `rotate()`, `skew()`, `skewX()`, `skewY()`, and `matrix()`; `transform-origin` with keywords, lengths, and percentages. Painting and `hit_test` use the transformed shape. Moves and even scales draw directly; other transforms are resampled with smoothed edges. A box's own clip turns with it; clips from its ancestors stay screen-aligned. A transform that cannot be inverted draws nothing and is never hit.
- Not supported: 3D functions, `perspective`, `transform-style`, `transform-box`, the standalone `translate`, `rotate`, and `scale` properties, and `calc()` inside a function. Transforms do not change layout or scrollable size, and they do not become the containing block for fixed or absolute descendants (those are still drawn through the transform). Inline boxes are not transformed. A rotated or skewed layer is drawn into a buffer of at most 2048 pixels per side.

### Opacity, transitions, and animations

- Supported: `opacity` (one group per box); `transition` and its longhands; `animation` and its longhands with `@keyframes`, iteration counts including `infinite`, all four directions, and all four fill modes; `ease`, `linear`, `ease-in`, `ease-out`, `ease-in-out`, `cubic-bezier()`, `steps()`, `step-start`, and `step-end`. Lengths, colors, opacity, and transforms blend, along with margins, padding, `border-radius`, `font-size`, `row-gap`, and `column-gap`. They run through `compute_styles_animated` with an `Animator`, whose `tick(now_ms)` takes the time from any clock and says whether another frame is needed.
- Not supported: transition and animation events, `animation-play-state`, `animation-composition`, `transition-behavior`, shortened reverse transitions, and blending `auto` with a length. `compute_styles` and the other plain cascade functions do not animate. At most 8 transitions and 4 animations per element.

## Build

Clone this repository and the libraries below as sibling directories. You don't need `anvil-browser` to build or test the engine.

```text
../anvil-engine
../anvil-image
../anvil-json
../anvil-ttf
../anvil-gpu
../anvil-compress
../anvil-css
../anvil-js
../anvil-video
../anvil-cipher
```

`anvil-image` links `anvil-compress`; `anvil-video` links `anvil-cipher`, `anvil-compress`, and `anvil-gpu`.

Rust 1.99.1 or newer. A C++20 compiler is optional. If none is found, the C++ files are skipped and the Rust paths are used.

```text
cargo test
```

## License

Dual-licensed under the **PolyForm Noncommercial License 1.0.0** (free for personal, educational, academic, and noncommercial auditing/research use) and a **Paid Commercial License with a Non-Compete Clause** (required for any commercial, for-profit, or business use). Competing browser, rendering engine, JS VM, or codec use is prohibited without a signed commercial agreement from Buntato. See [LICENSE](./LICENSE).
