// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0-OR-Commercial
// Copyright (c) 2026 Buntato
//! Integration tests for the self-hosted `anvil-js` compiler and Web/DOM runtime
//! wired into `anvil-script` and running on `AnvilVM`.

use anvil_dom::parse_html;
use anvil_script::{
    ScriptRuntime, compile_es2022_with_anvil_js, eval_es2022_with_anvil_js,
    eval_webapp_with_anvil_js, is_cpp_jit_kernel_active, self_hosted_bundle,
    self_hosted_bundle_hash, self_hosted_runtime_bundle,
};

#[test]
fn self_hosted_bundle_is_linked_into_anvil_script() {
    let bundle = self_hosted_bundle();
    assert!(bundle.len() > 400_000, "bundle length = {}", bundle.len());
    assert!(bundle.contains("function compileSource("));
    assert!(bundle.contains("function transpileToCoreJs("));
    assert!(bundle.contains("function createBrowserEnv("));
    assert_ne!(self_hosted_bundle_hash(), 0);
    assert!(self_hosted_runtime_bundle().contains("function createBrowserEnv("));
}

#[test]
fn self_hosted_es2022_lowering_and_execution_on_anvil_vm() {
    if !is_cpp_jit_kernel_active() {
        return;
    }

    let source = r#"
        class CounterBox {
            #count = 10;
            static base = 5;
            static {
                CounterBox.base = CounterBox.base + 2;
            }
            constructor(delta = 3, ...extra) {
                this.#count = this.#count + delta + CounterBox.base;
                for (const item of extra) {
                    this.#count = this.#count + item;
                }
            }
            read() {
                return this.#count;
            }
        }
        const box = new CounterBox(4, 3);
        const { a, b = 9 } = { a: box.read() };
        const [first, ...rest] = [a, b, 100];
        let cfg = null;
        cfg ??= first + rest[0] + rest[1];
        console.log("result=" + cfg);
    "#;

    let lowered = compile_es2022_with_anvil_js(source, false).expect("ES2022 source should lower");
    assert!(lowered.contains("class CounterBox"));
    assert!(lowered.contains("__priv_count"));

    let run = eval_es2022_with_anvil_js(source).expect("lowered script should run on AnvilVM");
    assert_eq!(run.output, vec!["result=133"]);
}

#[test]
fn search_portal_webapp_loads_and_filters_on_anvil_vm() {
    if !is_cpp_jit_kernel_active() {
        return;
    }

    let app_script = r#"
        class SearchController {
            #env = null;
            #results = [];
            constructor(env) {
                this.#env = env;
            }
            addEntry(title, url, snippet) {
                this.#results.push({ title, url, snippet });
            }
            renderFromLocation() {
                const loc = this.#env.window.location;
                const params = new AnvilURLSearchParams(loc.search);
                const q = params.get("q") ?? "";
                const doc = this.#env.document;
                const container = doc.createElement("div");
                container.id = "search-results";
                doc.body.appendChild(container);
                let matched = 0;
                for (const item of this.#results) {
                    if (stringIncludes(stringToLowerCase(item.title), stringToLowerCase(q))) {
                        const row = doc.createElement("article");
                        row.classList.add("result-card");
                        row.setAttribute("data-url", item.url);
                        row.textContent = item.title + " - " + item.snippet;
                        container.appendChild(row);
                        matched = matched + 1;
                    }
                }
                this.#env.window.history.pushState("q=" + q, "Search", "/search?q=" + q + "&count=" + matched);
                return matched;
            }
        }
        const ctrl = new SearchController(env);
        ctrl.addEntry("Anvil Engine Reference", "https://anvil.example/engine", "Self-hosted JS runtime");
        ctrl.addEntry("Weather Forecast", "https://anvil.example/weather", "Sunny skies");
        ctrl.addEntry("Anvil CSS Selectors", "https://anvil.example/css", "Level 4 selector matcher");
        const count = ctrl.renderFromLocation();
        console.log("matched=" + count);
        console.log("href=" + env.window.location.href);
        console.log("cards=" + env.document.querySelectorAll(".result-card").length);
    "#;

    let run = eval_webapp_with_anvil_js("https://search.example.com/search?q=anvil", app_script)
        .expect("search webapp should execute cleanly");
    assert_eq!(
        run.output,
        vec![
            "matched=2",
            "href=https://search.example.com/search?q=anvil&count=2",
            "cards=2",
        ]
    );
}

#[test]
fn video_portal_webapp_custom_elements_shadow_dom_and_mse_on_anvil_vm() {
    if !is_cpp_jit_kernel_active() {
        return;
    }

    let app_script = r#"
        class YtdAppElement {
            #title = "Untitled";
            constructor(hostNode, title = "Live Stream") {
                this.host = hostNode;
                this.#title = title;
            }
            mount(doc) {
                const shadow = this.host.attachShadow({ mode: "open" });
                const player = doc.createElement("div");
                player.id = "movie_player";
                player.classList.add("html5-video-player");
                player.textContent = this.#title;
                shadow.appendChild(player);
                return shadow;
            }
        }
        env.customElements.define("ytd-app", (hostNode, title) => new YtdAppElement(hostNode, title), null);
        const makeYtdApp = env.customElements.get("ytd-app");
        const host = env.document.createElement("ytd-app");
        env.document.body.appendChild(host);
        const instance = makeYtdApp(host, "4K 60fps Showcase");
        const shadow = instance.mount(env.document);

        const codecOk = AnvilMediaSource.isTypeSupported("video/mp4; codecs=\"avc1.640028\"");
        const ms = new AnvilMediaSource();
        const sb = ms.addSourceBuffer("video/mp4");
        sb.appendBuffer([0, 0, 0, 24, 102, 116, 121, 112]);
        ms.endOfStream();

        console.log("defined=" + (makeYtdApp !== null));
        console.log("shadowPlayer=" + shadow.getElementById("movie_player").getTextContent());
        console.log("codecOk=" + codecOk);
        console.log("msState=" + ms.readyState + ",segments=" + sb.segments.length);
    "#;

    let run = eval_webapp_with_anvil_js("https://video.example.com/watch?v=abc123", app_script)
        .expect("video portal webapp should execute cleanly");
    assert_eq!(
        run.output,
        vec![
            "defined=true",
            "shadowPlayer=4K 60fps Showcase",
            "codecOk=true",
            "msState=ended,segments=1",
        ]
    );
}

#[test]
fn webmail_inbox_webapp_json_storage_and_abort_controller_on_anvil_vm() {
    if !is_cpp_jit_kernel_active() {
        return;
    }

    let app_script = r#"
        class InboxStore {
            #storage = null;
            constructor(storage) {
                this.#storage = storage;
            }
            saveThreads(threads) {
                this.#storage.setItem("inbox_threads", anvilJsonStringify(threads));
            }
            loadUnread() {
                const raw = this.#storage.getItem("inbox_threads");
                if (raw === null) {
                    return [];
                }
                const all = anvilJsonParse(raw);
                return arrayFilter(all, (t) => t.unread === true);
            }
        }
        const store = new InboxStore(env.localStorage);
        store.saveThreads([
            { id: "t1", subject: "Release Checklist", unread: true },
            { id: "t2", subject: "Archive Digest", unread: false },
            { id: "t3", subject: "Security Advisory", unread: true }
        ]);
        const unread = store.loadUnread();
        for (const thread of unread) {
            const row = env.document.createElement("div");
            row.classList.add("mail-row", "unread");
            row.setAttribute("data-thread-id", thread.id);
            row.textContent = thread.subject;
            env.document.body.appendChild(row);
        }
        const ac = new AnvilAbortController();
        let abortedCount = 0;
        ac.signal.addEventListener("abort", () => {
            abortedCount = abortedCount + 1;
        });
        ac.abort("user-switched-folder");

        console.log("unread=" + env.document.querySelectorAll(".unread").length);
        console.log("first=" + unread[0]?.subject);
        console.log("aborted=" + ac.signal.aborted + ":" + abortedCount);
    "#;

    let run = eval_webapp_with_anvil_js("https://mail.example.com/mail/u/0/#inbox", app_script)
        .expect("webmail inbox webapp should execute cleanly");
    assert_eq!(
        run.output,
        vec!["unread=2", "first=Release Checklist", "aborted=true:1"]
    );
}

#[test]
fn script_runtime_dom_integration_supports_custom_elements_mse_and_location() {
    let html = r#"<!DOCTYPE html>
<html>
<head><title>Portal</title></head>
<body>
  <div id="app"></div>
  <ytd-app id="yt-root"></ytd-app>
</body>
</html>"#;
    let mut doc = parse_html(html);
    let mut rt = ScriptRuntime::new();
    rt.set_page_origin("https://video.example.com/watch?v=xyz987#t=30");

    let script = r#"
        customElements.define("ytd-app", "YtdAppController");
        let root = document.getElementById("yt-root");
        let shadow = root.attachShadow({ mode: "open" });
        shadow.innerHTML = "<span id='badge'>HD Ready</span>";
        let supported = MediaSource.isTypeSupported("video/mp4");
        let q = window.location.search;
        let path = window.location.pathname;
        let hash = window.location.hash;
        history.pushState({}, "", "/watch?v=next456");
        let nextHref = window.location.href;
        let app = document.getElementById("app");
        app.textContent = path + "|" + q + "|" + hash + "|" + supported + "|" + nextHref;
    "#;
    rt.eval_script(&mut doc, script);

    let app_id = doc.get_element_by_id("app").expect("#app exists");
    assert_eq!(
        doc.text_content(app_id),
        "/watch|?v=xyz987|#t=30|true|https://video.example.com:443/watch?v=next456"
    );
}

#[test]
fn automatic_anvil_vm_dom_routing_seeds_hierarchy_and_applies_mutations() {
    if !is_cpp_jit_kernel_active() {
        return;
    }

    let html = r#"<!DOCTYPE html>
<html>
<head><title>Initial</title></head>
<body>
  <section class="panel" data-section="main">
    <span class="badge">Old Badge</span>
    <input id="search-box" value="initial" />
  </section>
  <div id="target"></div>
</body>
</html>"#;
    let mut doc = parse_html(html);
    let mut rt = ScriptRuntime::new();
    rt.set_page_origin("https://portal.example.com/home");
    rt.local_storage_set("theme", "slate");

    let script = r#"
        class PortalUpdater {
            #prefix = "Portal:";
            static version = 1;
            static {
                PortalUpdater.version = PortalUpdater.version + 4;
            }
            apply() {
                const panel = document.querySelector(".panel");
                const badge = panel.querySelector(".badge");
                const sec = panel.getAttribute("data-section");
                const theme = localStorage.getItem("theme") ?? "light";
                badge.textContent = this.#prefix + sec + ":" + theme + ":v" + PortalUpdater.version;
                badge.classList.add("updated");
                const box = document.getElementById("search-box");
                box.setValue("anvil-query");
                const target = document.getElementById("target");
                target.setInnerHTML("<em>Ready</em>");
                target.insertAdjacentHTML("beforeend", "<strong>Now</strong>");
                localStorage.setItem("last_run", "ok");
                document.title = "Updated Portal";
            }
        }
        new PortalUpdater().apply();
    "#;

    rt.eval_script(&mut doc, script);
    assert!(rt.last_script_used_vm());
    assert_eq!(doc.title(), "Updated Portal");
    let badge = doc.query_selector(".badge").expect(".badge exists");
    assert_eq!(doc.text_content(badge), "Portal:main:slate:v5");
    assert!(doc.get(badge).data().has_class("updated"));
    let box_id = doc.get_element_by_id("search-box").expect("#search-box");
    assert_eq!(doc.input_value(box_id), "anvil-query");
    let target_id = doc.get_element_by_id("target").expect("#target");
    assert_eq!(doc.text_content(target_id), "ReadyNow");
    assert_eq!(rt.local_storage_get("last_run"), Some("ok"));
}

#[test]
fn interactive_anvil_vm_events_preserve_state_and_honor_prevent_default() {
    if !is_cpp_jit_kernel_active() {
        return;
    }

    let html = r#"<!DOCTYPE html>
<html>
<body>
  <form id="search-form" action="/search">
    <input id="q-input" name="q" value="" />
    <button id="inc-btn" type="button">+1</button>
    <button id="sub-btn" type="submit">Go</button>
  </form>
  <div id="status">0</div>
</body>
</html>"#;
    let mut doc = parse_html(html);
    let mut rt = ScriptRuntime::new();
    rt.set_page_origin("https://app.example.com/");

    let script = r#"
        class InteractiveApp {
            #clicks = 0;
            #query = "";
            constructor() {
                const btn = document.getElementById("inc-btn");
                const inp = document.getElementById("q-input");
                const form = document.getElementById("search-form");
                const status = document.getElementById("status");
                btn.addEventListener("click", () => {
                    this.#clicks = this.#clicks + 1;
                    status.setTextContent("clicks=" + this.#clicks + ",q=" + this.#query);
                });
                inp.addEventListener("input", (ev) => {
                    this.#query = inp.value;
                    status.setTextContent("clicks=" + this.#clicks + ",q=" + this.#query);
                });
                form.addEventListener("submit", (ev) => {
                    ev.preventDefault();
                    status.setTextContent("submitted:" + this.#query + ":" + this.#clicks);
                });
            }
        }
        new InteractiveApp();
    "#;

    rt.eval_script(&mut doc, script);
    assert!(rt.last_script_used_vm());

    let btn_id = doc.get_element_by_id("inc-btn").unwrap();
    let inp_id = doc.get_element_by_id("q-input").unwrap();
    let form_id = doc.get_element_by_id("search-form").unwrap();
    let status_id = doc.get_element_by_id("status").unwrap();

    assert!(rt.dispatch_click(&mut doc, btn_id));
    assert_eq!(doc.text_content(status_id), "clicks=1,q=");

    assert!(rt.dispatch_click(&mut doc, btn_id));
    assert_eq!(doc.text_content(status_id), "clicks=2,q=");

    assert!(rt.dispatch_input(&mut doc, inp_id, "rust"));
    assert_eq!(doc.text_content(status_id), "clicks=2,q=rust");

    assert!(rt.dispatch_submit(&mut doc, form_id));
    assert!(rt.last_event_default_prevented());
    assert_eq!(doc.text_content(status_id), "submitted:rust:2");
}

#[test]
fn mutation_intersection_and_resize_observers_deliver_microtask_callbacks() {
    let html = r#"<!DOCTYPE html>
<html>
<body>
  <div id="watched"><span id="child">Init</span></div>
  <div id="log"></div>
</body>
</html>"#;
    let mut doc = parse_html(html);
    let mut rt = ScriptRuntime::new();

    let script = r#"
        let events = [];
        let watched = document.getElementById("watched");
        let child = document.getElementById("child");
        let log = document.getElementById("log");

        let mo = new MutationObserver((records) => {
            events.push("mo:" + records.length + ":" + records[0].type + ":" + records[0].target.id);
            log.textContent = events.join("|");
        });
        mo.observe(watched, { childList: true, attributes: true, subtree: true });

        let io = new IntersectionObserver((entries) => {
            events.push("io:" + entries[0].isIntersecting + ":" + entries[0].target.id);
            log.textContent = events.join("|");
        });
        io.observe(watched);

        let ro = new ResizeObserver((entries) => {
            events.push("ro:" + entries[0].contentRect.width + ":" + entries[0].target.id);
            log.textContent = events.join("|");
        });
        ro.observe(watched);

        watched.setAttribute("data-state", "active");
        child.textContent = "Changed";
    "#;

    rt.eval_script(&mut doc, script);
    let _ = rt.run_timers(&mut doc);

    let log_id = doc.get_element_by_id("log").unwrap();
    let text = doc.text_content(log_id);
    assert!(text.contains("io:true:watched"), "got: {text}");
    assert!(text.contains("ro:100:watched"), "got: {text}");
    assert!(
        text.contains("mo:2:attributes:watched"),
        "expected MutationObserver callback in: {text}"
    );
}
