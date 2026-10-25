<div align="center">
  <h1>Anvil Engine</h1>
  <p>Page pipeline for Anvil Browser. Not the window, the network stack, or the profile file.</p>
  <p>
    <img src="https://img.shields.io/badge/release-v.22-ff6b24?style=flat-square" alt="v.22" />
    <img src="https://img.shields.io/badge/status-Proof_of_Concept-d4a359?style=flat-square" alt="Proof of Concept" />
    <a href="./LICENSE"><img src="https://img.shields.io/badge/license-Audit_and_use_only-e8c37a?style=flat-square" alt="Audit and use only" /></a>
    <img src="https://img.shields.io/badge/rust-2024-orange?style=flat-square" alt="Rust 2024" />
  </p>
</div>

Version 0.22. Author: Buntato.

There are no crates.io dependencies.

## Pipeline

| Crate | What it is |
| --- | --- |
| `anvil-dom` | HTML tree and selectors |
| `anvil-style` | CSS cascade and media queries |
| `anvil-layout` | Block, inline, flex, grid, table, and positioned boxes |
| `anvil-paint` | Display list and zoom |
| `anvil-raster` | RGBA framebuffer. Shadows, rounded boxes, and gradients use the software draw device |
| `anvil-script` | Budgeted script runtime and the numeric x86-64 compiler |

`anvil-script` does not open a socket. `fetch()` is performed by the host in `anvil-browser`.

## Scripts

`anvil-script` runs timers and promises on its own event loop. After each task the whole microtask queue is drained. Timers fire by due time, then in the order they were set, on a virtual clock that does not follow wall time. A timer set while a timer callback itself runs waits at least 4 ms. Timers set from the microtasks that callback queues, or after an `await` in an async timer callback, keep the delay they ask for.

Supported:

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

Limits: a turn spends at most 100,000 steps. Every microtask, timer callback, and resumed `await` costs steps, so a promise flood, a timer that sets itself again, or an endless `setInterval` is stopped with a reason (`abort_reason()`) instead of hanging. At most 64 timers and 8,192 queued microtasks are kept at once. Scripts that use the event loop always run on the Rust interpreter. The numeric compiler rejects them.

## CSS support

The crate docs list every property. These are the newer parts and their limits.

### Grid

- Supported: `grid-template-columns` and `grid-template-rows` with `px`, `%`, `fr`, `auto`, `minmax()`, and `repeat()`; `grid-auto-rows`; `grid-column`, `grid-row`, their `-start` / `-end` longhands, and numeric `grid-area`, with line numbers, negative lines, and `span N`; sparse row-by-row auto-placement that flows around spanning items; `align-items: center` and `end` inside grid areas.
- Not supported: named lines, `grid-template-areas` and named `grid-area`, `grid-auto-flow`, `grid-auto-columns`, `auto-fill` / `auto-fit`, `subgrid`, and implicit tracks before line 1 (such an item starts at line 1). Lines past 100 are clamped. An `auto` column acts as `1fr`.

### `ch`

- Supported on `width`, `height`, `max-width`, and the offsets. One `ch` is the advance of `0` in the system font when anvil-ttf loads one (Windows); otherwise it is half the font size, the advance layout uses for every character.
- Not supported: `ch` in margin, padding, border, `gap`, `font-size`, or `calc()`.

### `position: sticky`

- Supported: `top`, `right`, `bottom`, and `left` (`px`, `%`, and the other length units) against the viewport or the nearest ancestor with hidden overflow, bounded by the parent's content box. Viewport stickiness is applied at draw time for the scroll position, in every raster path and at every zoom; `hit_test_scrolled` finds boxes where they are drawn. `top` wins over `bottom` and `left` over `right`.
- Not supported: scrolling overflow containers (`overflow: auto` and `scroll` clip but do not scroll, so a sticky box inside one is placed once) and horizontal page scrolling.

### Transforms

- Supported: `transform` with up to 8 of `translate()`, `translateX()`, `translateY()`, `scale()`, `scaleX()`, `scaleY()`, `rotate()`, `skew()`, `skewX()`, `skewY()`, and `matrix()`; `transform-origin` with keywords, lengths, and percentages. Painting and `hit_test` use the transformed shape. Moves and even scales draw directly; other transforms are resampled with smoothed edges. A box's own clip turns with it; clips from its ancestors stay screen-aligned. A transform that cannot be inverted draws nothing and is never hit.
- Not supported: 3D functions, `perspective`, `transform-style`, `transform-box`, the standalone `translate`, `rotate`, and `scale` properties, and `calc()` inside a function. Transforms do not change layout or scrollable size, and they do not become the containing block for fixed or absolute descendants (those are still drawn through the transform). Inline boxes are not transformed. A rotated or skewed layer is drawn into a buffer of at most 2048 pixels per side.

### Opacity, transitions, and animations

- Supported: `opacity` (one group per box); `transition` and its longhands; `animation` and its longhands with `@keyframes`, iteration counts including `infinite`, all four directions, and all four fill modes; `ease`, `linear`, `ease-in`, `ease-out`, `ease-in-out`, `cubic-bezier()`, `steps()`, `step-start`, and `step-end`. Lengths, colors, opacity, and transforms blend, along with margins, padding, `border-radius`, `font-size`, and `gap`. They run through `compute_styles_animated` with an `Animator`, whose `tick(now_ms)` takes the time from any clock and says whether another frame is needed.
- Not supported: transition and animation events, `animation-play-state`, `animation-composition`, `transition-behavior`, shortened reverse transitions, and blending `auto` with a length. `compute_styles` and the other plain cascade functions do not animate. At most 8 transitions and 4 animations per element.

## Build

Clone this repository, `anvil-browser`, and the libraries below as sibling directories.

```text
../anvil-engine
../anvil-browser
../anvil-image
../anvil-json
../anvil-ttf
../anvil-gpu
../anvil-compress
```

`anvil-image` links `anvil-compress`.

Rust 1.85 or newer. A C++20 compiler is optional. If none is found, the C++ files are skipped and the Rust paths are used.

```text
cargo test
```

## License

This engine is private. You may read it to audit it and use it as part of Anvil Browser, but you may not redistribute it on its own. Anvil Browser can be redistributed without it. See `LICENSE`.

| Repository | License |
| --- | --- |
| This one (`anvil-dom`, `anvil-style`, `anvil-layout`, `anvil-paint`, `anvil-raster`, `anvil-script`) | Audit and use only. Not redistributable on its own |
| `anvil-browser` | Apache-2.0. Not this file |
| `anvil-image`, `anvil-json`, `anvil-ttf`, `anvil-gpu`, `anvil-compress` | MPL-2.0, in those repositories |

Revisions of these crates that were already published in `anvil-browser` under the Apache License 2.0 stay under that license for those revisions only.
