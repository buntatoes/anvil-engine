// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0-OR-Commercial
// Copyright (c) 2026 Buntato
//! Live DOM, `Storage`, `Location`, `Event`, and `console` host implementation
//! ([`AnvilJsDomHost`]) for the `anvil-js` bytecode virtual machine ([`anvil_js::vm::Vm`]).

use std::any::Any;
use std::collections::VecDeque;

use anvil_dom::{Document, NodeData, NodeId, parse_html};
use anvil_js::gc::roots::Persistent;
use anvil_js::value::object::{HostData, ObjectClass};
use anvil_js::value::{ObjectRef, Value as JsValue};
use anvil_js::vm::{Host, Job, NativeCx, NativeFn, RunError, Vm};

use crate::{
    camel_to_kebab, kebab_to_camel, read_inline_style, remove_inline_style_prop,
    update_inline_style,
};

const TAG_MASK: u64 = 0xF000_0000_0000_0000;
const TAG_NODE: u64 = 0x1000_0000_0000_0000;
const TAG_CLASS_LIST: u64 = 0x2000_0000_0000_0000;
const TAG_STYLE: u64 = 0x3000_0000_0000_0000;
const TAG_DATASET: u64 = 0x4000_0000_0000_0000;

/// Live DOM, `localStorage`, `sessionStorage`, `location`, event listener, and
/// microtask host for [`anvil_js::vm::Vm`].
#[derive(Debug)]
pub struct AnvilJsDomHost {
    /// Live HTML document mutated directly by JavaScript DOM operations.
    pub doc: Document,
    /// FIFO microtask queue for `Promise` reactions and `queueMicrotask`.
    pub jobs: VecDeque<Job>,
    /// Formatted `console.log` / `info` / `warn` / `error` output lines.
    pub console: Vec<String>,
    /// Active origin's `localStorage` entries.
    pub local_storage: Vec<(String, String)>,
    /// Active origin's `sessionStorage` entries.
    pub session_storage: Vec<(String, String)>,
    /// Current page URL (`window.location.href` / `document.URL`).
    pub location_href: String,
    /// Pending navigation URL requested by `location.assign` / `replace` / `href = ...`.
    pub pending_navigation: Option<String>,
    /// True when the active event handler called `event.preventDefault()`.
    pub event_default_prevented: bool,
    /// True when the active event handler called `event.stopPropagation()`.
    pub event_propagation_stopped: bool,
    /// Monotonic clock in milliseconds for `Date.now()` and `performance.now()`.
    pub clock_ms: f64,
    /// Prototype for wrapped DOM `Element` / `Node` instances.
    pub element_proto: Option<ObjectRef>,
    /// Prototype for `element.classList` `DOMTokenList` wrappers.
    pub class_list_proto: Option<ObjectRef>,
    /// Prototype for `element.style` `CSSStyleDeclaration` wrappers.
    pub style_proto: Option<ObjectRef>,
    /// Prototype for `element.dataset` `DOMStringMap` wrappers.
    pub dataset_proto: Option<ObjectRef>,
    /// Global `document` object.
    pub document_obj: Option<ObjectRef>,
    /// Registered live DOM event listeners: `(target_node, event_type_lowercase, callback)`.
    pub listeners: Vec<(NodeId, String, Persistent)>,
}

impl Default for AnvilJsDomHost {
    fn default() -> Self {
        Self::new(Document::new(), "https://localhost/")
    }
}

impl AnvilJsDomHost {
    /// Create a new [`AnvilJsDomHost`] owning `doc` and `location_href`.
    pub fn new(doc: Document, location_href: &str) -> Self {
        Self {
            doc,
            jobs: VecDeque::new(),
            console: Vec::new(),
            local_storage: Vec::new(),
            session_storage: Vec::new(),
            location_href: if location_href.trim().is_empty() {
                "https://localhost/".to_string()
            } else {
                location_href.trim().to_string()
            },
            pending_navigation: None,
            event_default_prevented: false,
            event_propagation_stopped: false,
            clock_ms: 1_700_000_000_000.0,
            element_proto: None,
            class_list_proto: None,
            style_proto: None,
            dataset_proto: None,
            document_obj: None,
            listeners: Vec::new(),
        }
    }

    /// Install all Web/DOM globals (`window`, `document`, `Element.prototype`,
    /// `localStorage`, `sessionStorage`, `location`, `navigator`, `customElements`,
    /// `DOMParser`, `CSSStyleSheet`, `matchMedia`, `getComputedStyle`) onto `vm`.
    pub fn install(&mut self, vm: &mut Vm) -> Result<(), RunError> {
        let g = vm.global();

        // 1. Element.prototype
        let el_proto = vm.new_plain_object()?;
        for (name, func) in [
            ("getAttribute", el_get_attribute as NativeFn),
            ("setAttribute", el_set_attribute),
            ("removeAttribute", el_remove_attribute),
            ("hasAttribute", el_has_attribute),
            ("appendChild", el_append_child),
            ("removeChild", el_remove_child),
            ("insertBefore", el_insert_before),
            ("replaceChild", el_replace_child),
            ("cloneNode", el_clone_node),
            ("remove", el_remove),
            ("querySelector", el_query_selector),
            ("querySelectorAll", el_query_selector_all),
            ("getElementById", el_get_element_by_id),
            ("getElementsByTagName", el_get_elements_by_tag_name),
            ("getElementsByClassName", el_get_elements_by_class_name),
            ("insertAdjacentHTML", el_insert_adjacent_html),
            ("insertAdjacentText", el_insert_adjacent_text),
            ("insertAdjacentElement", el_insert_adjacent_element),
            ("matches", el_matches),
            ("closest", el_closest),
            ("contains", el_contains),
            ("attachShadow", el_attach_shadow),
            ("addEventListener", el_add_event_listener),
            ("getBoundingClientRect", el_get_bounding_client_rect),
            ("getContext", el_get_context),
            ("setTextContent", el_set_text_content_method),
            ("getTextContent", el_get_text_content_method),
            ("setInnerHTML", el_set_inner_html_method),
            ("setValue", el_set_value_method),
            ("focus", el_noop),
            ("blur", el_noop),
            ("scrollIntoView", el_noop),
        ] {
            vm.define_method(el_proto, name, func)?;
        }

        for (prop, getter, setter) in [
            (
                "id",
                Some(el_get_id as NativeFn),
                Some(el_set_id as NativeFn),
            ),
            (
                "className",
                Some(el_get_class_name),
                Some(el_set_class_name),
            ),
            (
                "textContent",
                Some(el_get_text_content),
                Some(el_set_text_content),
            ),
            (
                "innerText",
                Some(el_get_text_content),
                Some(el_set_text_content),
            ),
            (
                "innerHTML",
                Some(el_get_inner_html),
                Some(el_set_inner_html),
            ),
            ("outerHTML", Some(el_get_outer_html), None),
            ("value", Some(el_get_value), Some(el_set_value)),
            ("checked", Some(el_get_checked), Some(el_set_checked)),
            ("tagName", Some(el_get_tag_name), None),
            ("nodeName", Some(el_get_tag_name), None),
            ("parentElement", Some(el_get_parent_element), None),
            ("parentNode", Some(el_get_parent_element), None),
            ("firstElementChild", Some(el_get_first_element_child), None),
            ("lastElementChild", Some(el_get_last_element_child), None),
            ("firstChild", Some(el_get_first_child), None),
            ("lastChild", Some(el_get_last_child), None),
            (
                "nextElementSibling",
                Some(el_get_next_element_sibling),
                None,
            ),
            (
                "previousElementSibling",
                Some(el_get_previous_element_sibling),
                None,
            ),
            ("children", Some(el_get_children), None),
            ("childElementCount", Some(el_get_child_element_count), None),
            ("shadowRoot", Some(el_get_shadow_root), None),
            ("classList", Some(el_get_class_list), None),
            ("style", Some(el_get_style), None),
            ("dataset", Some(el_get_dataset), None),
        ] {
            vm.define_accessor(el_proto, prop, getter, setter)?;
        }
        self.element_proto = Some(el_proto);

        // 2. DOMTokenList (element.classList) prototype
        let cl_proto = vm.new_plain_object()?;
        for (name, func) in [
            ("add", cl_add as NativeFn),
            ("remove", cl_remove),
            ("toggle", cl_toggle),
            ("contains", cl_contains),
            ("toString", cl_to_string),
        ] {
            vm.define_method(cl_proto, name, func)?;
        }
        vm.define_accessor(cl_proto, "value", Some(cl_to_string), None)?;
        self.class_list_proto = Some(cl_proto);

        // 3. CSSStyleDeclaration (element.style) prototype
        let st_proto = vm.new_plain_object()?;
        for (name, func) in [
            ("setProperty", style_set_property as NativeFn),
            ("getPropertyValue", style_get_property_value),
            ("removeProperty", style_remove_property),
        ] {
            vm.define_method(st_proto, name, func)?;
        }
        vm.define_accessor(
            st_proto,
            "cssText",
            Some(style_get_css_text),
            Some(style_set_css_text),
        )?;
        for prop in [
            "color",
            "backgroundColor",
            "background",
            "display",
            "width",
            "height",
            "minWidth",
            "minHeight",
            "maxWidth",
            "maxHeight",
            "opacity",
            "transform",
            "fontSize",
            "fontWeight",
            "margin",
            "marginTop",
            "marginBottom",
            "marginLeft",
            "marginRight",
            "padding",
            "paddingTop",
            "paddingBottom",
            "paddingLeft",
            "paddingRight",
            "border",
            "borderRadius",
            "visibility",
            "position",
            "top",
            "left",
            "right",
            "bottom",
            "zIndex",
            "overflow",
            "cursor",
            "textAlign",
            "lineHeight",
            "gap",
            "flexDirection",
            "alignItems",
            "justifyContent",
        ] {
            self.install_style_accessor(vm, st_proto, prop)?;
        }
        self.style_proto = Some(st_proto);

        // 4. DOMStringMap (element.dataset) prototype
        let ds_proto = vm.new_plain_object()?;
        vm.define_method(ds_proto, "get", dataset_get)?;
        vm.define_method(ds_proto, "set", dataset_set)?;
        for key in [
            "id",
            "index",
            "value",
            "state",
            "mode",
            "tab",
            "section",
            "url",
            "action",
            "target",
            "type",
            "name",
            "role",
            "theme",
            "status",
            "count",
            "active",
            "open",
            "loaded",
            "recovered",
            "threadId",
            "testid",
        ] {
            self.install_dataset_accessor(vm, ds_proto, key)?;
        }
        self.dataset_proto = Some(ds_proto);

        // 5. document object
        let doc_obj = vm.new_plain_object()?;
        for (name, func) in [
            ("getElementById", doc_get_element_by_id as NativeFn),
            ("querySelector", doc_query_selector),
            ("querySelectorAll", doc_query_selector_all),
            ("createElement", doc_create_element),
            ("createTextNode", doc_create_text_node),
            ("createComment", doc_create_text_node),
            ("createDocumentFragment", doc_create_document_fragment),
            ("getElementsByTagName", doc_get_elements_by_tag_name),
            ("getElementsByClassName", doc_get_elements_by_class_name),
            ("getElementsByName", doc_get_elements_by_name),
            ("addEventListener", doc_add_event_listener),
        ] {
            vm.define_method(doc_obj, name, func)?;
        }
        vm.define_accessor(doc_obj, "title", Some(doc_get_title), Some(doc_set_title))?;
        vm.define_accessor(doc_obj, "body", Some(doc_get_body), None)?;
        vm.define_accessor(doc_obj, "head", Some(doc_get_head), None)?;
        vm.define_accessor(
            doc_obj,
            "documentElement",
            Some(doc_get_document_element),
            None,
        )?;
        vm.define_accessor(doc_obj, "readyState", Some(doc_get_ready_state), None)?;
        vm.define_accessor(doc_obj, "URL", Some(doc_get_url), None)?;
        vm.define_accessor(doc_obj, "documentURI", Some(doc_get_url), None)?;
        self.document_obj = Some(doc_obj);

        // 6. localStorage & sessionStorage
        let local_obj = vm.new_plain_object()?;
        for (name, func) in [
            ("getItem", local_storage_get_item as NativeFn),
            ("setItem", local_storage_set_item),
            ("removeItem", local_storage_remove_item),
            ("clear", local_storage_clear),
            ("key", local_storage_key),
        ] {
            vm.define_method(local_obj, name, func)?;
        }
        vm.define_accessor(local_obj, "length", Some(local_storage_length), None)?;

        let session_obj = vm.new_plain_object()?;
        for (name, func) in [
            ("getItem", session_storage_get_item as NativeFn),
            ("setItem", session_storage_set_item),
            ("removeItem", session_storage_remove_item),
            ("clear", session_storage_clear),
            ("key", session_storage_key),
        ] {
            vm.define_method(session_obj, name, func)?;
        }
        vm.define_accessor(session_obj, "length", Some(session_storage_length), None)?;

        // 7. location, history, navigator, customElements
        let loc_obj = vm.new_plain_object()?;
        vm.define_accessor(loc_obj, "href", Some(loc_get_href), Some(loc_set_href))?;
        vm.define_accessor(loc_obj, "origin", Some(loc_get_origin), None)?;
        vm.define_accessor(loc_obj, "pathname", Some(loc_get_pathname), None)?;
        vm.define_accessor(loc_obj, "search", Some(loc_get_search), None)?;
        vm.define_accessor(loc_obj, "hash", Some(loc_get_hash), None)?;
        vm.define_method(loc_obj, "assign", loc_assign)?;
        vm.define_method(loc_obj, "replace", loc_assign)?;
        vm.define_method(loc_obj, "toString", loc_get_href)?;

        let hist_obj = vm.new_plain_object()?;
        vm.define_method(hist_obj, "pushState", hist_push_state)?;
        vm.define_method(hist_obj, "replaceState", hist_push_state)?;

        let nav_obj = vm.new_plain_object()?;
        let ua = vm.string("AnvilBrowser/0.24")?;
        let lang = vm.string("en-US")?;
        vm.define_data(nav_obj, "userAgent", ua)?;
        vm.define_data(nav_obj, "language", lang)?;

        let ce_obj = vm.new_plain_object()?;
        vm.define_method(ce_obj, "define", custom_elements_define)?;
        vm.define_method(ce_obj, "get", custom_elements_get)?;

        // 8. Global bindings
        vm.define_data(g, "window", JsValue::object(g))?;
        vm.define_data(g, "self", JsValue::object(g))?;
        vm.define_data(g, "document", JsValue::object(doc_obj))?;
        vm.define_data(g, "localStorage", JsValue::object(local_obj))?;
        vm.define_data(g, "sessionStorage", JsValue::object(session_obj))?;
        vm.define_data(g, "location", JsValue::object(loc_obj))?;
        vm.define_data(g, "history", JsValue::object(hist_obj))?;
        vm.define_data(g, "navigator", JsValue::object(nav_obj))?;
        vm.define_data(g, "customElements", JsValue::object(ce_obj))?;
        vm.define_method(g, "getComputedStyle", window_get_computed_style)?;
        vm.define_method(g, "matchMedia", window_match_media)?;
        vm.define_method(g, "addEventListener", doc_add_event_listener)?;

        Ok(())
    }

    fn install_style_accessor(
        &self,
        vm: &mut Vm,
        st_proto: ObjectRef,
        prop: &str,
    ) -> Result<(), RunError> {
        let css_name = camel_to_kebab(prop);
        let getter_src = format!(
            "(function(prop) {{ return function() {{ return this.getPropertyValue(prop); }}; }})({:?})",
            css_name
        );
        let setter_src = format!(
            "(function(prop) {{ return function(val) {{ this.setProperty(prop, val); }}; }})({:?})",
            css_name
        );
        let mut dummy_host = anvil_js::vm::BasicHost::default();
        let limits = vm.limits().clone();
        let get_code = anvil_js::compile_script(&getter_src, &limits)
            .map_err(|e| RunError::internal(e.to_string()))?;
        let get_id = vm.load(get_code)?;
        let get_fn = vm.run(&mut dummy_host, get_id)?;

        let set_code = anvil_js::compile_script(&setter_src, &limits)
            .map_err(|e| RunError::internal(e.to_string()))?;
        let set_id = vm.load(set_code)?;
        let set_fn = vm.run(&mut dummy_host, set_id)?;

        let key = vm
            .heap_mut()
            .key_from_str(prop)
            .map_err(|e| RunError::internal(format!("{e:?}")))?;
        vm.heap_mut()
            .define_own(
                st_proto,
                key,
                anvil_js::value::object::Property {
                    attrs: anvil_js::value::shape::Attrs(
                        anvil_js::value::shape::Attrs::ENUMERABLE
                            | anvil_js::value::shape::Attrs::CONFIGURABLE,
                    ),
                    slot: anvil_js::value::object::Slot::Accessor {
                        get: get_fn,
                        set: set_fn,
                    },
                },
            )
            .map_err(|e| RunError::internal(format!("{e:?}")))?;
        Ok(())
    }

    fn install_dataset_accessor(
        &self,
        vm: &mut Vm,
        ds_proto: ObjectRef,
        key_name: &str,
    ) -> Result<(), RunError> {
        let getter_src = format!(
            "(function(k) {{ return function() {{ return this.get(k); }}; }})({:?})",
            key_name
        );
        let setter_src = format!(
            "(function(k) {{ return function(v) {{ this.set(k, v); }}; }})({:?})",
            key_name
        );
        let mut dummy_host = anvil_js::vm::BasicHost::default();
        let limits = vm.limits().clone();
        let get_code = anvil_js::compile_script(&getter_src, &limits)
            .map_err(|e| RunError::internal(e.to_string()))?;
        let get_id = vm.load(get_code)?;
        let get_fn = vm.run(&mut dummy_host, get_id)?;

        let set_code = anvil_js::compile_script(&setter_src, &limits)
            .map_err(|e| RunError::internal(e.to_string()))?;
        let set_id = vm.load(set_code)?;
        let set_fn = vm.run(&mut dummy_host, set_id)?;

        let key = vm
            .heap_mut()
            .key_from_str(key_name)
            .map_err(|e| RunError::internal(format!("{e:?}")))?;
        vm.heap_mut()
            .define_own(
                ds_proto,
                key,
                anvil_js::value::object::Property {
                    attrs: anvil_js::value::shape::Attrs(
                        anvil_js::value::shape::Attrs::ENUMERABLE
                            | anvil_js::value::shape::Attrs::CONFIGURABLE,
                    ),
                    slot: anvil_js::value::object::Slot::Accessor {
                        get: get_fn,
                        set: set_fn,
                    },
                },
            )
            .map_err(|e| RunError::internal(format!("{e:?}")))?;
        Ok(())
    }

    /// Dispatch a live DOM event (`click`, `input`, `change`, `submit`, `keydown`)
    /// along the target's ancestor chain in `self.doc`, invoking all matching
    /// JS callbacks registered via `addEventListener` and draining microtasks.
    pub fn dispatch_event(
        &mut self,
        vm: &mut Vm,
        target: NodeId,
        event_type: &str,
        value: Option<&str>,
        key: Option<&str>,
    ) -> Result<bool, RunError> {
        self.event_default_prevented = false;
        self.event_propagation_stopped = false;
        let wanted = event_type.trim().to_ascii_lowercase();
        let mut chain = Vec::new();
        let mut cursor = Some(target);
        while let Some(id) = cursor {
            chain.push(id);
            cursor = self.doc.get(id).parent();
        }
        if !chain.contains(&self.doc.body()) {
            chain.push(self.doc.body());
        }
        if !chain.contains(&self.doc.root()) {
            chain.push(self.doc.root());
        }

        let target_val = self.wrap_node_in_vm(vm, target)?;
        let mut handled = false;

        for current_id in chain {
            if self.event_propagation_stopped {
                break;
            }
            let matching: Vec<Persistent> = self
                .listeners
                .iter()
                .filter(|(nid, ev, _)| {
                    *nid == current_id && (ev == &wanted || (wanted == "input" && ev == "change"))
                })
                .map(|(_, _, cb)| *cb)
                .collect();
            if matching.is_empty() {
                continue;
            }
            let cur_val = self.wrap_node_in_vm(vm, current_id)?;
            for cb_pers in matching {
                let cb_val = vm
                    .heap()
                    .persistent(cb_pers)
                    .map_err(|e| RunError::internal(format!("{e:?}")))?;
                let ev_obj = vm.new_plain_object()?;
                let t_str = vm.string(&wanted)?;
                vm.define_data(ev_obj, "type", t_str)?;
                vm.define_data(ev_obj, "target", target_val)?;
                vm.define_data(ev_obj, "currentTarget", cur_val)?;
                vm.define_data(ev_obj, "bubbles", JsValue::TRUE)?;
                vm.define_data(ev_obj, "cancelable", JsValue::TRUE)?;
                if let Some(v) = value {
                    let vs = vm.string(v)?;
                    vm.define_data(ev_obj, "value", vs)?;
                    vm.define_data(ev_obj, "data", vs)?;
                }
                if let Some(k) = key {
                    let ks = vm.string(k)?;
                    vm.define_data(ev_obj, "key", ks)?;
                    vm.define_data(ev_obj, "code", ks)?;
                }
                vm.define_accessor(
                    ev_obj,
                    "defaultPrevented",
                    Some(ev_get_default_prevented),
                    None,
                )?;
                vm.define_method(ev_obj, "preventDefault", ev_prevent_default)?;
                vm.define_method(ev_obj, "stopPropagation", ev_stop_propagation)?;

                let ret = vm.call(self, cb_val, cur_val, &[JsValue::object(ev_obj)])?;
                if ret.as_bool() == Some(false) {
                    self.event_default_prevented = true;
                }
                handled = true;
                vm.drain_jobs(self)?;
            }
        }
        Ok(handled)
    }

    fn wrap_node_in_vm(&self, vm: &mut Vm, nid: NodeId) -> Result<JsValue, RunError> {
        let data = HostData(TAG_NODE | (nid.index() as u64));
        if let Some(existing) = vm.heap().wrapper(data) {
            return Ok(JsValue::object(existing));
        }
        let obj = vm.new_host_object(self.element_proto, data)?;
        vm.heap_mut().set_wrapper(data, obj);
        Ok(JsValue::object(obj))
    }
}

impl Host for AnvilJsDomHost {
    fn enqueue_job(&mut self, job: Job) {
        self.jobs.push_back(job);
    }

    fn take_job(&mut self) -> Option<Job> {
        self.jobs.pop_front()
    }

    fn trace(&self, roots: &mut Vec<JsValue>) {
        for proto in [
            self.element_proto,
            self.class_list_proto,
            self.style_proto,
            self.dataset_proto,
            self.document_obj,
        ]
        .into_iter()
        .flatten()
        {
            roots.push(JsValue::object(proto));
        }
    }

    fn now_ms(&mut self) -> f64 {
        let cur = self.clock_ms;
        self.clock_ms += 1.0;
        cur
    }

    fn console(&mut self, level: &str, text: &str) {
        let prefix = match level {
            "warn" => "[warn] ",
            "error" => "[error] ",
            _ => "",
        };
        self.console.push(format!("{prefix}{text}"));
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

// ---- Host/Wrapper helpers -------------------------------------------------

fn wrap_node(cx: &mut NativeCx<'_>, nid: NodeId) -> Result<JsValue, RunError> {
    let data = HostData(TAG_NODE | (nid.index() as u64));
    if let Some(existing) = cx.heap().wrapper(data) {
        return Ok(JsValue::object(existing));
    }
    let proto = cx.host_as::<AnvilJsDomHost>().and_then(|h| h.element_proto);
    let obj = cx.vm().new_host_object(proto, data)?;
    cx.heap().set_wrapper(data, obj);
    Ok(JsValue::object(obj))
}

fn wrap_optional_node(cx: &mut NativeCx<'_>, nid: Option<NodeId>) -> Result<JsValue, RunError> {
    match nid {
        Some(id) => wrap_node(cx, id),
        None => Ok(JsValue::NULL),
    }
}

fn wrap_node_list(cx: &mut NativeCx<'_>, ids: Vec<NodeId>) -> Result<JsValue, RunError> {
    let mut vals = Vec::with_capacity(ids.len());
    for id in ids {
        let v = wrap_node(cx, id)?;
        cx.keep(v);
        vals.push(v);
    }
    cx.vm().new_array_from(&vals)
}

fn unwrap_node(cx: &mut NativeCx<'_>, val: JsValue) -> Option<NodeId> {
    let obj = val.as_object()?;
    if let Some(doc_obj) = cx.host_as::<AnvilJsDomHost>().and_then(|h| h.document_obj)
        && obj == doc_obj
    {
        return cx.host_as::<AnvilJsDomHost>().map(|h| h.doc.root());
    }
    let class = cx.heap().object(obj).ok()?.class.clone();
    match class {
        ObjectClass::Host(HostData(raw)) => {
            let tag = raw & TAG_MASK;
            if matches!(tag, TAG_NODE | TAG_CLASS_LIST | TAG_STYLE | TAG_DATASET) {
                Some(NodeId::from_raw((raw & 0xFFFF_FFFF) as u32))
            } else {
                None
            }
        }
        _ => None,
    }
}

fn arg_str(cx: &mut NativeCx<'_>, args: &[JsValue], idx: usize) -> Result<String, RunError> {
    match args.get(idx).copied() {
        Some(v) if !v.is_undefined() => cx.to_display_string(v),
        _ => Ok(String::new()),
    }
}

// ---- Event methods --------------------------------------------------------

fn ev_get_default_prevented(
    cx: &mut NativeCx<'_>,
    _this: JsValue,
    _args: &[JsValue],
) -> Result<JsValue, RunError> {
    let prevented = cx
        .host_as::<AnvilJsDomHost>()
        .is_some_and(|h| h.event_default_prevented);
    Ok(JsValue::boolean(prevented))
}

fn ev_prevent_default(
    cx: &mut NativeCx<'_>,
    _this: JsValue,
    _args: &[JsValue],
) -> Result<JsValue, RunError> {
    if let Some(h) = cx.host_as::<AnvilJsDomHost>() {
        h.event_default_prevented = true;
    }
    Ok(JsValue::UNDEFINED)
}

fn ev_stop_propagation(
    cx: &mut NativeCx<'_>,
    _this: JsValue,
    _args: &[JsValue],
) -> Result<JsValue, RunError> {
    if let Some(h) = cx.host_as::<AnvilJsDomHost>() {
        h.event_propagation_stopped = true;
    }
    Ok(JsValue::UNDEFINED)
}

// ---- Document methods & accessors -----------------------------------------

fn doc_get_element_by_id(
    cx: &mut NativeCx<'_>,
    _this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let id_str = arg_str(cx, args, 0)?;
    let found = cx
        .host_as::<AnvilJsDomHost>()
        .and_then(|h| h.doc.get_element_by_id(&id_str));
    wrap_optional_node(cx, found)
}

fn doc_query_selector(
    cx: &mut NativeCx<'_>,
    _this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let sel = arg_str(cx, args, 0)?;
    let found = cx
        .host_as::<AnvilJsDomHost>()
        .and_then(|h| h.doc.query_selector(&sel));
    wrap_optional_node(cx, found)
}

fn doc_query_selector_all(
    cx: &mut NativeCx<'_>,
    _this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let sel = arg_str(cx, args, 0)?;
    let ids = cx
        .host_as::<AnvilJsDomHost>()
        .map(|h| h.doc.query_selector_all(&sel))
        .unwrap_or_default();
    wrap_node_list(cx, ids)
}

fn doc_create_element(
    cx: &mut NativeCx<'_>,
    _this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let tag = arg_str(cx, args, 0)?;
    let tag = if tag.trim().is_empty() {
        "div"
    } else {
        tag.trim()
    };
    let Some(h) = cx.host_as::<AnvilJsDomHost>() else {
        return Ok(JsValue::NULL);
    };
    let nid = h.doc.create_element(tag, Vec::new());
    wrap_node(cx, nid)
}

fn doc_create_text_node(
    cx: &mut NativeCx<'_>,
    _this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let text = arg_str(cx, args, 0)?;
    let Some(h) = cx.host_as::<AnvilJsDomHost>() else {
        return Ok(JsValue::NULL);
    };
    let nid = h.doc.create_text(&text);
    wrap_node(cx, nid)
}

fn doc_create_document_fragment(
    cx: &mut NativeCx<'_>,
    _this: JsValue,
    _args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(h) = cx.host_as::<AnvilJsDomHost>() else {
        return Ok(JsValue::NULL);
    };
    let nid = h.doc.create_element("anvil-fragment", Vec::new());
    wrap_node(cx, nid)
}

fn doc_get_elements_by_tag_name(
    cx: &mut NativeCx<'_>,
    _this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let tag = arg_str(cx, args, 0)?;
    let ids = cx
        .host_as::<AnvilJsDomHost>()
        .map(|h| h.doc.get_elements_by_tag_name(h.doc.root(), &tag))
        .unwrap_or_default();
    wrap_node_list(cx, ids)
}

fn doc_get_elements_by_class_name(
    cx: &mut NativeCx<'_>,
    _this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let cls = arg_str(cx, args, 0)?;
    let ids = cx
        .host_as::<AnvilJsDomHost>()
        .map(|h| h.doc.get_elements_by_class_name(h.doc.root(), &cls))
        .unwrap_or_default();
    wrap_node_list(cx, ids)
}

fn doc_get_elements_by_name(
    cx: &mut NativeCx<'_>,
    _this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let name = arg_str(cx, args, 0)?;
    let ids = cx
        .host_as::<AnvilJsDomHost>()
        .map(|h| h.doc.get_elements_by_name(&name))
        .unwrap_or_default();
    wrap_node_list(cx, ids)
}

fn doc_add_event_listener(
    cx: &mut NativeCx<'_>,
    _this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let ev_type = arg_str(cx, args, 0)?.trim().to_ascii_lowercase();
    let Some(cb) = args.get(1).copied().filter(|v| v.is_object()) else {
        return Ok(JsValue::UNDEFINED);
    };
    if matches!(
        ev_type.as_str(),
        "domcontentloaded" | "load" | "readystatechange"
    ) {
        cx.enqueue_job(cb, JsValue::UNDEFINED, &[])?;
        return Ok(JsValue::UNDEFINED);
    }
    let pers = cx
        .heap()
        .persist(cb)
        .map_err(|e| RunError::internal(format!("{e:?}")))?;
    if let Some(h) = cx.host_as::<AnvilJsDomHost>() {
        let root = h.doc.root();
        h.listeners.push((root, ev_type, pers));
    }
    Ok(JsValue::UNDEFINED)
}

fn doc_get_title(
    cx: &mut NativeCx<'_>,
    _this: JsValue,
    _args: &[JsValue],
) -> Result<JsValue, RunError> {
    let title = cx
        .host_as::<AnvilJsDomHost>()
        .map(|h| h.doc.title())
        .unwrap_or_default();
    cx.vm().string(&title)
}

fn doc_set_title(
    cx: &mut NativeCx<'_>,
    _this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let title = arg_str(cx, args, 0)?;
    if let Some(h) = cx.host_as::<AnvilJsDomHost>() {
        h.doc.set_title(&title);
    }
    Ok(JsValue::UNDEFINED)
}

fn doc_get_body(
    cx: &mut NativeCx<'_>,
    _this: JsValue,
    _args: &[JsValue],
) -> Result<JsValue, RunError> {
    let body = cx.host_as::<AnvilJsDomHost>().map(|h| h.doc.body());
    wrap_optional_node(cx, body)
}

fn doc_get_head(
    cx: &mut NativeCx<'_>,
    _this: JsValue,
    _args: &[JsValue],
) -> Result<JsValue, RunError> {
    let head = cx.host_as::<AnvilJsDomHost>().map(|h| h.doc.head());
    wrap_optional_node(cx, head)
}

fn doc_get_document_element(
    cx: &mut NativeCx<'_>,
    _this: JsValue,
    _args: &[JsValue],
) -> Result<JsValue, RunError> {
    let html = cx.host_as::<AnvilJsDomHost>().map(|h| h.doc.html());
    wrap_optional_node(cx, html)
}

fn doc_get_ready_state(
    cx: &mut NativeCx<'_>,
    _this: JsValue,
    _args: &[JsValue],
) -> Result<JsValue, RunError> {
    cx.vm().string("complete")
}

fn doc_get_url(
    cx: &mut NativeCx<'_>,
    _this: JsValue,
    _args: &[JsValue],
) -> Result<JsValue, RunError> {
    let url = cx
        .host_as::<AnvilJsDomHost>()
        .map(|h| h.location_href.clone())
        .unwrap_or_default();
    cx.vm().string(&url)
}

// ---- Element methods & accessors ------------------------------------------

fn el_get_attribute(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return Ok(JsValue::NULL);
    };
    let name = arg_str(cx, args, 0)?;
    let val = cx
        .host_as::<AnvilJsDomHost>()
        .and_then(|h| h.doc.get(nid).data().attr(&name).map(str::to_string));
    match val {
        Some(s) => cx.vm().string(&s),
        None => Ok(JsValue::NULL),
    }
}

fn el_set_attribute(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return Ok(JsValue::UNDEFINED);
    };
    let name = arg_str(cx, args, 0)?;
    let val = arg_str(cx, args, 1)?;
    if let Some(h) = cx.host_as::<AnvilJsDomHost>() {
        h.doc.set_attribute(nid, &name, &val);
    }
    Ok(JsValue::UNDEFINED)
}

fn el_remove_attribute(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return Ok(JsValue::UNDEFINED);
    };
    let name = arg_str(cx, args, 0)?;
    if let Some(h) = cx.host_as::<AnvilJsDomHost>() {
        h.doc.remove_attribute(nid, &name);
    }
    Ok(JsValue::UNDEFINED)
}

fn el_has_attribute(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return Ok(JsValue::FALSE);
    };
    let name = arg_str(cx, args, 0)?;
    let has = cx
        .host_as::<AnvilJsDomHost>()
        .is_some_and(|h| h.doc.get(nid).data().attr(&name).is_some());
    Ok(JsValue::boolean(has))
}

fn el_append_child(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(parent_id) = unwrap_node(cx, this) else {
        return Ok(JsValue::NULL);
    };
    let child_val = args.first().copied().unwrap_or(JsValue::NULL);
    let Some(child_id) = unwrap_node(cx, child_val) else {
        return Ok(child_val);
    };
    if let Some(h) = cx.host_as::<AnvilJsDomHost>() {
        if h.doc.get(child_id).data().tag() == Some("anvil-fragment") {
            let kids: Vec<NodeId> = h.doc.children(child_id).to_vec();
            for k in kids {
                let _ = h.doc.append(parent_id, k);
            }
        } else {
            let _ = h.doc.append(parent_id, child_id);
        }
    }
    Ok(child_val)
}

fn el_remove_child(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(parent_id) = unwrap_node(cx, this) else {
        return Ok(JsValue::NULL);
    };
    let child_val = args.first().copied().unwrap_or(JsValue::NULL);
    if let Some(child_id) = unwrap_node(cx, child_val)
        && let Some(h) = cx.host_as::<AnvilJsDomHost>()
    {
        let _ = h.doc.remove_child(parent_id, child_id);
    }
    Ok(child_val)
}

fn el_insert_before(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(parent_id) = unwrap_node(cx, this) else {
        return Ok(JsValue::NULL);
    };
    let new_val = args.first().copied().unwrap_or(JsValue::NULL);
    let Some(new_id) = unwrap_node(cx, new_val) else {
        return Ok(new_val);
    };
    let ref_id = args.get(1).copied().and_then(|v| unwrap_node(cx, v));
    if let Some(h) = cx.host_as::<AnvilJsDomHost>() {
        let _ = h.doc.insert_before(parent_id, new_id, ref_id);
    }
    Ok(new_val)
}

fn el_replace_child(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(parent_id) = unwrap_node(cx, this) else {
        return Ok(JsValue::NULL);
    };
    let new_val = args.first().copied().unwrap_or(JsValue::NULL);
    let old_val = args.get(1).copied().unwrap_or(JsValue::NULL);
    if let (Some(new_id), Some(old_id)) = (unwrap_node(cx, new_val), unwrap_node(cx, old_val))
        && let Some(h) = cx.host_as::<AnvilJsDomHost>()
    {
        let _ = h.doc.replace_child(parent_id, new_id, old_id);
    }
    Ok(old_val)
}

fn el_clone_node(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return Ok(JsValue::NULL);
    };
    let deep = args
        .first()
        .copied()
        .is_some_and(|v| v.as_bool() == Some(true));
    let cloned = cx
        .host_as::<AnvilJsDomHost>()
        .and_then(|h| h.doc.clone_node(nid, deep));
    wrap_optional_node(cx, cloned)
}

fn el_remove(cx: &mut NativeCx<'_>, this: JsValue, _args: &[JsValue]) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return Ok(JsValue::UNDEFINED);
    };
    if let Some(h) = cx.host_as::<AnvilJsDomHost>() {
        h.doc.remove_node(nid);
    }
    Ok(JsValue::UNDEFINED)
}

fn el_query_selector(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return Ok(JsValue::NULL);
    };
    let sel = arg_str(cx, args, 0)?;
    let found = cx.host_as::<AnvilJsDomHost>().and_then(|h| {
        if nid == h.doc.root() {
            h.doc.query_selector(&sel)
        } else {
            h.doc.query_selector_within(nid, &sel)
        }
    });
    wrap_optional_node(cx, found)
}

fn el_query_selector_all(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return cx.vm().new_array_from(&[]);
    };
    let sel = arg_str(cx, args, 0)?;
    let ids = cx
        .host_as::<AnvilJsDomHost>()
        .map(|h| {
            if nid == h.doc.root() {
                h.doc.query_selector_all(&sel)
            } else {
                h.doc.query_selector_all_within(nid, &sel)
            }
        })
        .unwrap_or_default();
    wrap_node_list(cx, ids)
}

fn el_get_element_by_id(
    cx: &mut NativeCx<'_>,
    _this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    doc_get_element_by_id(cx, _this, args)
}

fn el_get_elements_by_tag_name(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return cx.vm().new_array_from(&[]);
    };
    let tag = arg_str(cx, args, 0)?;
    let ids = cx
        .host_as::<AnvilJsDomHost>()
        .map(|h| h.doc.get_elements_by_tag_name(nid, &tag))
        .unwrap_or_default();
    wrap_node_list(cx, ids)
}

fn el_get_elements_by_class_name(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return cx.vm().new_array_from(&[]);
    };
    let cls = arg_str(cx, args, 0)?;
    let ids = cx
        .host_as::<AnvilJsDomHost>()
        .map(|h| h.doc.get_elements_by_class_name(nid, &cls))
        .unwrap_or_default();
    wrap_node_list(cx, ids)
}

fn el_insert_adjacent_html(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return Ok(JsValue::UNDEFINED);
    };
    let pos = arg_str(cx, args, 0)?;
    let html = arg_str(cx, args, 1)?;
    if let Some(h) = cx.host_as::<AnvilJsDomHost>() {
        h.doc.insert_adjacent_html(nid, &pos, &html);
    }
    Ok(JsValue::UNDEFINED)
}

fn el_insert_adjacent_text(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return Ok(JsValue::UNDEFINED);
    };
    let pos = arg_str(cx, args, 0)?;
    let text = arg_str(cx, args, 1)?;
    if let Some(h) = cx.host_as::<AnvilJsDomHost>() {
        h.doc.insert_adjacent_text(nid, &pos, &text);
    }
    Ok(JsValue::UNDEFINED)
}

fn el_insert_adjacent_element(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return Ok(JsValue::NULL);
    };
    let pos = arg_str(cx, args, 0)?;
    let Some(new_el) = args.get(1).copied().and_then(|v| unwrap_node(cx, v)) else {
        return Ok(JsValue::NULL);
    };
    let inserted = cx
        .host_as::<AnvilJsDomHost>()
        .and_then(|h| h.doc.insert_adjacent_element(nid, &pos, new_el));
    wrap_optional_node(cx, inserted)
}

fn el_matches(cx: &mut NativeCx<'_>, this: JsValue, args: &[JsValue]) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return Ok(JsValue::FALSE);
    };
    let sel = arg_str(cx, args, 0)?;
    let matched = cx
        .host_as::<AnvilJsDomHost>()
        .is_some_and(|h| h.doc.matches_selector(nid, &sel));
    Ok(JsValue::boolean(matched))
}

fn el_closest(cx: &mut NativeCx<'_>, this: JsValue, args: &[JsValue]) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return Ok(JsValue::NULL);
    };
    let sel = arg_str(cx, args, 0)?;
    let found = cx
        .host_as::<AnvilJsDomHost>()
        .and_then(|h| h.doc.closest(nid, &sel));
    wrap_optional_node(cx, found)
}

fn el_contains(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return Ok(JsValue::FALSE);
    };
    let Some(other_id) = args.first().copied().and_then(|v| unwrap_node(cx, v)) else {
        return Ok(JsValue::FALSE);
    };
    let contains = cx
        .host_as::<AnvilJsDomHost>()
        .is_some_and(|h| h.doc.contains_node(nid, other_id));
    Ok(JsValue::boolean(contains))
}

fn el_attach_shadow(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    _args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return Ok(JsValue::NULL);
    };
    let sr = cx
        .host_as::<AnvilJsDomHost>()
        .map(|h| h.doc.attach_shadow(nid, true));
    wrap_optional_node(cx, sr)
}

fn el_add_event_listener(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return Ok(JsValue::UNDEFINED);
    };
    let ev_type = arg_str(cx, args, 0)?.trim().to_ascii_lowercase();
    let Some(cb) = args.get(1).copied().filter(|v| v.is_object()) else {
        return Ok(JsValue::UNDEFINED);
    };
    let pers = cx
        .heap()
        .persist(cb)
        .map_err(|e| RunError::internal(format!("{e:?}")))?;
    if let Some(h) = cx.host_as::<AnvilJsDomHost>() {
        h.listeners.push((nid, ev_type, pers));
    }
    Ok(JsValue::UNDEFINED)
}

fn el_get_bounding_client_rect(
    cx: &mut NativeCx<'_>,
    _this: JsValue,
    _args: &[JsValue],
) -> Result<JsValue, RunError> {
    let rect = cx.vm().new_plain_object()?;
    for (k, v) in [
        ("x", 0.0),
        ("y", 0.0),
        ("top", 0.0),
        ("left", 0.0),
        ("width", 100.0),
        ("height", 24.0),
        ("right", 100.0),
        ("bottom", 24.0),
    ] {
        cx.vm().define_data(rect, k, JsValue::number(v))?;
    }
    Ok(JsValue::object(rect))
}

fn el_get_context(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    _args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return Ok(JsValue::NULL);
    };
    let ctx_obj = cx
        .vm()
        .new_host_object(None, HostData(TAG_NODE | (nid.index() as u64)))?;
    let black = cx.vm().string("#000000")?;
    cx.vm().define_data(ctx_obj, "fillStyle", black)?;
    cx.vm().define_data(ctx_obj, "strokeStyle", black)?;
    cx.vm()
        .define_data(ctx_obj, "lineWidth", JsValue::number(1.0))?;
    for (name, func) in [
        ("fillRect", canvas_fill_rect as NativeFn),
        ("strokeRect", canvas_stroke_rect),
        ("clearRect", canvas_clear_rect),
        ("fillText", canvas_fill_text),
    ] {
        cx.vm().define_method(ctx_obj, name, func)?;
    }
    Ok(JsValue::object(ctx_obj))
}

fn canvas_fill_rect(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return Ok(JsValue::UNDEFINED);
    };
    let x = args.first().and_then(|v| v.as_number()).unwrap_or(0.0) as i32;
    let y = args.get(1).and_then(|v| v.as_number()).unwrap_or(0.0) as i32;
    let w = args
        .get(2)
        .and_then(|v| v.as_number())
        .unwrap_or(0.0)
        .max(0.0) as u32;
    let h = args
        .get(3)
        .and_then(|v| v.as_number())
        .unwrap_or(0.0)
        .max(0.0) as u32;
    let style_str = if let Some(obj) = this.as_object()
        && let Ok(key) = cx.heap().key_from_str("fillStyle")
        && let Ok(Some(prop)) = cx.heap().get_own(obj, key)
        && let Some(val) = prop.slot.data()
    {
        cx.to_display_string(val)
            .unwrap_or_else(|_| "#000000".to_string())
    } else {
        "#000000".to_string()
    };
    let rgba = crate::parse_canvas_color(&style_str);
    if let Some(host) = cx.host_as::<AnvilJsDomHost>() {
        host.doc.canvas_fill_rect(nid, x, y, w, h, rgba);
    }
    Ok(JsValue::UNDEFINED)
}

fn canvas_stroke_rect(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return Ok(JsValue::UNDEFINED);
    };
    let x = args.first().and_then(|v| v.as_number()).unwrap_or(0.0) as i32;
    let y = args.get(1).and_then(|v| v.as_number()).unwrap_or(0.0) as i32;
    let w = args
        .get(2)
        .and_then(|v| v.as_number())
        .unwrap_or(0.0)
        .max(0.0) as u32;
    let h = args
        .get(3)
        .and_then(|v| v.as_number())
        .unwrap_or(0.0)
        .max(0.0) as u32;
    let style_str = if let Some(obj) = this.as_object()
        && let Ok(key) = cx.heap().key_from_str("strokeStyle")
        && let Ok(Some(prop)) = cx.heap().get_own(obj, key)
        && let Some(val) = prop.slot.data()
    {
        cx.to_display_string(val)
            .unwrap_or_else(|_| "#000000".to_string())
    } else {
        "#000000".to_string()
    };
    let rgba = crate::parse_canvas_color(&style_str);
    if let Some(host) = cx.host_as::<AnvilJsDomHost>() {
        host.doc.canvas_stroke_rect(nid, x, y, w, h, 1, rgba);
    }
    Ok(JsValue::UNDEFINED)
}

fn canvas_clear_rect(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return Ok(JsValue::UNDEFINED);
    };
    let x = args.first().and_then(|v| v.as_number()).unwrap_or(0.0) as i32;
    let y = args.get(1).and_then(|v| v.as_number()).unwrap_or(0.0) as i32;
    let w = args
        .get(2)
        .and_then(|v| v.as_number())
        .unwrap_or(0.0)
        .max(0.0) as u32;
    let h = args
        .get(3)
        .and_then(|v| v.as_number())
        .unwrap_or(0.0)
        .max(0.0) as u32;
    if let Some(host) = cx.host_as::<AnvilJsDomHost>() {
        host.doc.canvas_clear_rect(nid, x, y, w, h);
    }
    Ok(JsValue::UNDEFINED)
}

fn canvas_fill_text(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return Ok(JsValue::UNDEFINED);
    };
    let text = arg_str(cx, args, 0)?;
    let x = args.get(1).and_then(|v| v.as_number()).unwrap_or(0.0) as i32;
    let y = args.get(2).and_then(|v| v.as_number()).unwrap_or(0.0) as i32;
    if let Some(host) = cx.host_as::<AnvilJsDomHost>() {
        host.doc
            .canvas_draw_text_bitmap(nid, &text, x, y, [0, 0, 0, 255]);
    }
    Ok(JsValue::UNDEFINED)
}

fn el_noop(_cx: &mut NativeCx<'_>, _this: JsValue, _args: &[JsValue]) -> Result<JsValue, RunError> {
    Ok(JsValue::UNDEFINED)
}

fn el_get_id(cx: &mut NativeCx<'_>, this: JsValue, _args: &[JsValue]) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return cx.vm().string("");
    };
    let id = cx
        .host_as::<AnvilJsDomHost>()
        .and_then(|h| h.doc.get(nid).data().id().map(str::to_string))
        .unwrap_or_default();
    cx.vm().string(&id)
}

fn el_set_id(cx: &mut NativeCx<'_>, this: JsValue, args: &[JsValue]) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return Ok(JsValue::UNDEFINED);
    };
    let val = arg_str(cx, args, 0)?;
    if let Some(h) = cx.host_as::<AnvilJsDomHost>() {
        h.doc.set_attribute(nid, "id", &val);
    }
    Ok(JsValue::UNDEFINED)
}

fn el_get_class_name(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    _args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return cx.vm().string("");
    };
    let cls = cx
        .host_as::<AnvilJsDomHost>()
        .and_then(|h| h.doc.get(nid).data().attr("class").map(str::to_string))
        .unwrap_or_default();
    cx.vm().string(&cls)
}

fn el_set_class_name(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return Ok(JsValue::UNDEFINED);
    };
    let val = arg_str(cx, args, 0)?;
    if let Some(h) = cx.host_as::<AnvilJsDomHost>() {
        h.doc.set_attribute(nid, "class", &val);
    }
    Ok(JsValue::UNDEFINED)
}

fn el_get_text_content(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    _args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return cx.vm().string("");
    };
    let text = cx
        .host_as::<AnvilJsDomHost>()
        .map(|h| h.doc.text_content(nid))
        .unwrap_or_default();
    cx.vm().string(&text)
}

fn el_set_text_content(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return Ok(JsValue::UNDEFINED);
    };
    let text = arg_str(cx, args, 0)?;
    if let Some(h) = cx.host_as::<AnvilJsDomHost>() {
        h.doc.set_text_content(nid, &text);
    }
    Ok(JsValue::UNDEFINED)
}

fn el_get_text_content_method(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    el_get_text_content(cx, this, args)
}

fn el_set_text_content_method(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    el_set_text_content(cx, this, args)
}

fn el_get_inner_html(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    _args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return cx.vm().string("");
    };
    let html = cx
        .host_as::<AnvilJsDomHost>()
        .map(|h| h.doc.inner_html(nid))
        .unwrap_or_default();
    cx.vm().string(&html)
}

fn el_set_inner_html(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return Ok(JsValue::UNDEFINED);
    };
    let html = arg_str(cx, args, 0)?;
    if let Some(h) = cx.host_as::<AnvilJsDomHost>() {
        h.doc.set_inner_html(nid, &html);
    }
    Ok(JsValue::UNDEFINED)
}

fn el_set_inner_html_method(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    el_set_inner_html(cx, this, args)
}

fn el_get_outer_html(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    _args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return cx.vm().string("");
    };
    let html = cx
        .host_as::<AnvilJsDomHost>()
        .map(|h| h.doc.outer_html(nid))
        .unwrap_or_default();
    cx.vm().string(&html)
}

fn el_get_value(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    _args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return cx.vm().string("");
    };
    let val = cx
        .host_as::<AnvilJsDomHost>()
        .map(|h| h.doc.input_value(nid))
        .unwrap_or_default();
    cx.vm().string(&val)
}

fn el_set_value(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return Ok(JsValue::UNDEFINED);
    };
    let val = arg_str(cx, args, 0)?;
    if let Some(h) = cx.host_as::<AnvilJsDomHost>() {
        h.doc.set_input_value(nid, &val);
    }
    Ok(JsValue::UNDEFINED)
}

fn el_set_value_method(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    el_set_value(cx, this, args)
}

fn el_get_checked(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    _args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return Ok(JsValue::FALSE);
    };
    let checked = cx
        .host_as::<AnvilJsDomHost>()
        .is_some_and(|h| h.doc.is_checked(nid));
    Ok(JsValue::boolean(checked))
}

fn el_set_checked(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return Ok(JsValue::UNDEFINED);
    };
    let checked = args
        .first()
        .copied()
        .is_some_and(|v| v.as_bool() == Some(true) || v.as_number().is_some_and(|n| n != 0.0));
    if let Some(h) = cx.host_as::<AnvilJsDomHost>() {
        h.doc.set_checked(nid, checked);
    }
    Ok(JsValue::UNDEFINED)
}

fn el_get_tag_name(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    _args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return cx.vm().string("");
    };
    let tag = cx
        .host_as::<AnvilJsDomHost>()
        .and_then(|h| h.doc.get(nid).data().tag().map(str::to_ascii_uppercase))
        .unwrap_or_default();
    cx.vm().string(&tag)
}

fn el_get_parent_element(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    _args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return Ok(JsValue::NULL);
    };
    let parent = cx
        .host_as::<AnvilJsDomHost>()
        .and_then(|h| h.doc.parent_element(nid));
    wrap_optional_node(cx, parent)
}

fn el_get_first_element_child(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    _args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return Ok(JsValue::NULL);
    };
    let first = cx
        .host_as::<AnvilJsDomHost>()
        .and_then(|h| h.doc.first_element_child(nid));
    wrap_optional_node(cx, first)
}

fn el_get_last_element_child(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    _args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return Ok(JsValue::NULL);
    };
    let last = cx
        .host_as::<AnvilJsDomHost>()
        .and_then(|h| h.doc.last_element_child(nid));
    wrap_optional_node(cx, last)
}

fn el_get_first_child(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    _args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return Ok(JsValue::NULL);
    };
    let first = cx
        .host_as::<AnvilJsDomHost>()
        .and_then(|h| h.doc.first_child(nid));
    wrap_optional_node(cx, first)
}

fn el_get_last_child(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    _args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return Ok(JsValue::NULL);
    };
    let last = cx
        .host_as::<AnvilJsDomHost>()
        .and_then(|h| h.doc.last_child(nid));
    wrap_optional_node(cx, last)
}

fn el_get_next_element_sibling(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    _args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return Ok(JsValue::NULL);
    };
    let next = cx
        .host_as::<AnvilJsDomHost>()
        .and_then(|h| h.doc.next_element_sibling(nid));
    wrap_optional_node(cx, next)
}

fn el_get_previous_element_sibling(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    _args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return Ok(JsValue::NULL);
    };
    let prev = cx
        .host_as::<AnvilJsDomHost>()
        .and_then(|h| h.doc.previous_element_sibling(nid));
    wrap_optional_node(cx, prev)
}

fn el_get_children(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    _args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return cx.vm().new_array_from(&[]);
    };
    let kids = cx
        .host_as::<AnvilJsDomHost>()
        .map(|h| h.doc.element_children(nid))
        .unwrap_or_default();
    wrap_node_list(cx, kids)
}

fn el_get_child_element_count(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    _args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return Ok(JsValue::number(0.0));
    };
    let count = cx
        .host_as::<AnvilJsDomHost>()
        .map(|h| h.doc.element_children(nid).len())
        .unwrap_or(0);
    Ok(JsValue::number(count as f64))
}

fn el_get_shadow_root(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    _args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return Ok(JsValue::NULL);
    };
    let sr = cx
        .host_as::<AnvilJsDomHost>()
        .and_then(|h| h.doc.shadow_root(nid));
    wrap_optional_node(cx, sr)
}

fn el_get_class_list(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    _args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return Ok(JsValue::NULL);
    };
    let data = HostData(TAG_CLASS_LIST | (nid.index() as u64));
    if let Some(existing) = cx.heap().wrapper(data) {
        return Ok(JsValue::object(existing));
    }
    let proto = cx
        .host_as::<AnvilJsDomHost>()
        .and_then(|h| h.class_list_proto);
    let obj = cx.vm().new_host_object(proto, data)?;
    cx.heap().set_wrapper(data, obj);
    Ok(JsValue::object(obj))
}

fn el_get_style(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    _args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return Ok(JsValue::NULL);
    };
    let data = HostData(TAG_STYLE | (nid.index() as u64));
    if let Some(existing) = cx.heap().wrapper(data) {
        return Ok(JsValue::object(existing));
    }
    let proto = cx.host_as::<AnvilJsDomHost>().and_then(|h| h.style_proto);
    let obj = cx.vm().new_host_object(proto, data)?;
    cx.heap().set_wrapper(data, obj);
    Ok(JsValue::object(obj))
}

fn el_get_dataset(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    _args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return Ok(JsValue::NULL);
    };
    let data = HostData(TAG_DATASET | (nid.index() as u64));
    if let Some(existing) = cx.heap().wrapper(data) {
        return Ok(JsValue::object(existing));
    }
    let proto = cx.host_as::<AnvilJsDomHost>().and_then(|h| h.dataset_proto);
    let obj = cx.vm().new_host_object(proto, data)?;
    cx.heap().set_wrapper(data, obj);
    Ok(JsValue::object(obj))
}

// ---- classList methods ----------------------------------------------------

fn cl_add(cx: &mut NativeCx<'_>, this: JsValue, args: &[JsValue]) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return Ok(JsValue::UNDEFINED);
    };
    for i in 0..args.len() {
        let token = arg_str(cx, args, i)?;
        if let Some(h) = cx.host_as::<AnvilJsDomHost>() {
            h.doc.add_class(nid, token.trim());
        }
    }
    Ok(JsValue::UNDEFINED)
}

fn cl_remove(cx: &mut NativeCx<'_>, this: JsValue, args: &[JsValue]) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return Ok(JsValue::UNDEFINED);
    };
    for i in 0..args.len() {
        let token = arg_str(cx, args, i)?;
        if let Some(h) = cx.host_as::<AnvilJsDomHost>() {
            h.doc.remove_class(nid, token.trim());
        }
    }
    Ok(JsValue::UNDEFINED)
}

fn cl_toggle(cx: &mut NativeCx<'_>, this: JsValue, args: &[JsValue]) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return Ok(JsValue::FALSE);
    };
    let token = arg_str(cx, args, 0)?;
    let present = if let Some(force) = args.get(1).copied()
        && !force.is_undefined()
    {
        let on = force.as_bool() == Some(true) || force.as_number().is_some_and(|n| n != 0.0);
        if let Some(h) = cx.host_as::<AnvilJsDomHost>() {
            if on {
                h.doc.add_class(nid, token.trim());
            } else {
                h.doc.remove_class(nid, token.trim());
            }
        }
        on
    } else {
        cx.host_as::<AnvilJsDomHost>()
            .is_some_and(|h| h.doc.toggle_class(nid, token.trim()))
    };
    Ok(JsValue::boolean(present))
}

fn cl_contains(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return Ok(JsValue::FALSE);
    };
    let token = arg_str(cx, args, 0)?;
    let has = cx
        .host_as::<AnvilJsDomHost>()
        .is_some_and(|h| h.doc.get(nid).data().has_class(token.trim()));
    Ok(JsValue::boolean(has))
}

fn cl_to_string(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    el_get_class_name(cx, this, args)
}

// ---- style methods --------------------------------------------------------

fn style_set_property(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return Ok(JsValue::UNDEFINED);
    };
    let raw_prop = arg_str(cx, args, 0)?;
    let css_prop = camel_to_kebab(raw_prop.trim());
    let val = arg_str(cx, args, 1)?;
    if let Some(h) = cx.host_as::<AnvilJsDomHost>() {
        if val.trim().is_empty() {
            remove_inline_style_prop(&mut h.doc, nid, &css_prop);
        } else {
            update_inline_style(&mut h.doc, nid, &css_prop, val.trim());
        }
    }
    Ok(JsValue::UNDEFINED)
}

fn style_get_property_value(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return cx.vm().string("");
    };
    let raw_prop = arg_str(cx, args, 0)?;
    let css_prop = camel_to_kebab(raw_prop.trim());
    let val = cx
        .host_as::<AnvilJsDomHost>()
        .map(|h| read_inline_style(&h.doc, nid, &css_prop))
        .unwrap_or_default();
    cx.vm().string(&val)
}

fn style_remove_property(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return cx.vm().string("");
    };
    let raw_prop = arg_str(cx, args, 0)?;
    let css_prop = camel_to_kebab(raw_prop.trim());
    let prev = cx
        .host_as::<AnvilJsDomHost>()
        .map(|h| {
            let old = read_inline_style(&h.doc, nid, &css_prop);
            remove_inline_style_prop(&mut h.doc, nid, &css_prop);
            old
        })
        .unwrap_or_default();
    cx.vm().string(&prev)
}

fn style_get_css_text(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    _args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return cx.vm().string("");
    };
    let css = cx
        .host_as::<AnvilJsDomHost>()
        .and_then(|h| h.doc.get(nid).data().attr("style").map(str::to_string))
        .unwrap_or_default();
    cx.vm().string(&css)
}

fn style_set_css_text(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return Ok(JsValue::UNDEFINED);
    };
    let css = arg_str(cx, args, 0)?;
    if let Some(h) = cx.host_as::<AnvilJsDomHost>() {
        h.doc.set_attribute(nid, "style", &css);
    }
    Ok(JsValue::UNDEFINED)
}

// ---- dataset methods ------------------------------------------------------

fn dataset_get(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return Ok(JsValue::UNDEFINED);
    };
    let key = arg_str(cx, args, 0)?;
    let attr_name = format!("data-{}", camel_to_kebab(key.trim()));
    let val = cx
        .host_as::<AnvilJsDomHost>()
        .and_then(|h| h.doc.get(nid).data().attr(&attr_name).map(str::to_string));
    match val {
        Some(s) => cx.vm().string(&s),
        None => Ok(JsValue::UNDEFINED),
    }
}

fn dataset_set(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(nid) = unwrap_node(cx, this) else {
        return Ok(JsValue::UNDEFINED);
    };
    let key = arg_str(cx, args, 0)?;
    let val = arg_str(cx, args, 1)?;
    let attr_name = format!("data-{}", camel_to_kebab(key.trim()));
    if let Some(h) = cx.host_as::<AnvilJsDomHost>() {
        h.doc.set_attribute(nid, &attr_name, &val);
    }
    Ok(JsValue::UNDEFINED)
}

// ---- Storage methods ------------------------------------------------------

fn local_storage_get_item(
    cx: &mut NativeCx<'_>,
    _this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let key = arg_str(cx, args, 0)?;
    let val = cx.host_as::<AnvilJsDomHost>().and_then(|h| {
        h.local_storage
            .iter()
            .find(|(k, _)| k == &key)
            .map(|(_, v)| v.clone())
    });
    match val {
        Some(s) => cx.vm().string(&s),
        None => Ok(JsValue::NULL),
    }
}

fn local_storage_set_item(
    cx: &mut NativeCx<'_>,
    _this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let key = arg_str(cx, args, 0)?;
    let val = arg_str(cx, args, 1)?;
    if let Some(h) = cx.host_as::<AnvilJsDomHost>() {
        if let Some((_, existing)) = h.local_storage.iter_mut().find(|(k, _)| k == &key) {
            *existing = val;
        } else {
            h.local_storage.push((key, val));
        }
    }
    Ok(JsValue::UNDEFINED)
}

fn local_storage_remove_item(
    cx: &mut NativeCx<'_>,
    _this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let key = arg_str(cx, args, 0)?;
    if let Some(h) = cx.host_as::<AnvilJsDomHost>() {
        h.local_storage.retain(|(k, _)| k != &key);
    }
    Ok(JsValue::UNDEFINED)
}

fn local_storage_clear(
    cx: &mut NativeCx<'_>,
    _this: JsValue,
    _args: &[JsValue],
) -> Result<JsValue, RunError> {
    if let Some(h) = cx.host_as::<AnvilJsDomHost>() {
        h.local_storage.clear();
    }
    Ok(JsValue::UNDEFINED)
}

fn local_storage_key(
    cx: &mut NativeCx<'_>,
    _this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let idx = args.first().and_then(|v| v.as_number()).unwrap_or(0.0) as usize;
    let key = cx
        .host_as::<AnvilJsDomHost>()
        .and_then(|h| h.local_storage.get(idx).map(|(k, _)| k.clone()));
    match key {
        Some(s) => cx.vm().string(&s),
        None => Ok(JsValue::NULL),
    }
}

fn local_storage_length(
    cx: &mut NativeCx<'_>,
    _this: JsValue,
    _args: &[JsValue],
) -> Result<JsValue, RunError> {
    let len = cx
        .host_as::<AnvilJsDomHost>()
        .map(|h| h.local_storage.len())
        .unwrap_or(0);
    Ok(JsValue::number(len as f64))
}

fn session_storage_get_item(
    cx: &mut NativeCx<'_>,
    _this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let key = arg_str(cx, args, 0)?;
    let val = cx.host_as::<AnvilJsDomHost>().and_then(|h| {
        h.session_storage
            .iter()
            .find(|(k, _)| k == &key)
            .map(|(_, v)| v.clone())
    });
    match val {
        Some(s) => cx.vm().string(&s),
        None => Ok(JsValue::NULL),
    }
}

fn session_storage_set_item(
    cx: &mut NativeCx<'_>,
    _this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let key = arg_str(cx, args, 0)?;
    let val = arg_str(cx, args, 1)?;
    if let Some(h) = cx.host_as::<AnvilJsDomHost>() {
        if let Some((_, existing)) = h.session_storage.iter_mut().find(|(k, _)| k == &key) {
            *existing = val;
        } else {
            h.session_storage.push((key, val));
        }
    }
    Ok(JsValue::UNDEFINED)
}

fn session_storage_remove_item(
    cx: &mut NativeCx<'_>,
    _this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let key = arg_str(cx, args, 0)?;
    if let Some(h) = cx.host_as::<AnvilJsDomHost>() {
        h.session_storage.retain(|(k, _)| k != &key);
    }
    Ok(JsValue::UNDEFINED)
}

fn session_storage_clear(
    cx: &mut NativeCx<'_>,
    _this: JsValue,
    _args: &[JsValue],
) -> Result<JsValue, RunError> {
    if let Some(h) = cx.host_as::<AnvilJsDomHost>() {
        h.session_storage.clear();
    }
    Ok(JsValue::UNDEFINED)
}

fn session_storage_key(
    cx: &mut NativeCx<'_>,
    _this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let idx = args.first().and_then(|v| v.as_number()).unwrap_or(0.0) as usize;
    let key = cx
        .host_as::<AnvilJsDomHost>()
        .and_then(|h| h.session_storage.get(idx).map(|(k, _)| k.clone()));
    match key {
        Some(s) => cx.vm().string(&s),
        None => Ok(JsValue::NULL),
    }
}

fn session_storage_length(
    cx: &mut NativeCx<'_>,
    _this: JsValue,
    _args: &[JsValue],
) -> Result<JsValue, RunError> {
    let len = cx
        .host_as::<AnvilJsDomHost>()
        .map(|h| h.session_storage.len())
        .unwrap_or(0);
    Ok(JsValue::number(len as f64))
}

// ---- Location, History, CustomElements & Window helpers -------------------

fn loc_get_href(
    cx: &mut NativeCx<'_>,
    _this: JsValue,
    _args: &[JsValue],
) -> Result<JsValue, RunError> {
    let href = cx
        .host_as::<AnvilJsDomHost>()
        .map(|h| h.location_href.clone())
        .unwrap_or_default();
    cx.vm().string(&href)
}

fn loc_set_href(
    cx: &mut NativeCx<'_>,
    _this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let next = arg_str(cx, args, 0)?;
    if let Some(h) = cx.host_as::<AnvilJsDomHost>() {
        let resolved = crate::resolve_script_fetch_url(&h.location_href, &next);
        h.location_href = resolved.clone();
        h.pending_navigation = Some(resolved);
    }
    Ok(JsValue::UNDEFINED)
}

fn loc_assign(cx: &mut NativeCx<'_>, this: JsValue, args: &[JsValue]) -> Result<JsValue, RunError> {
    loc_set_href(cx, this, args)
}

fn loc_get_origin(
    cx: &mut NativeCx<'_>,
    _this: JsValue,
    _args: &[JsValue],
) -> Result<JsValue, RunError> {
    let href = cx
        .host_as::<AnvilJsDomHost>()
        .map(|h| h.location_href.clone())
        .unwrap_or_default();
    let origin = if let Some((scheme, host, port)) = crate::extract_origin_tuple(&href) {
        if (scheme == "https" && port == 443) || (scheme == "http" && port == 80) {
            format!("{scheme}://{host}")
        } else {
            format!("{scheme}://{host}:{port}")
        }
    } else {
        href
    };
    cx.vm().string(&origin)
}

fn loc_get_pathname(
    cx: &mut NativeCx<'_>,
    _this: JsValue,
    _args: &[JsValue],
) -> Result<JsValue, RunError> {
    let href = cx
        .host_as::<AnvilJsDomHost>()
        .map(|h| h.location_href.clone())
        .unwrap_or_default();
    let after_scheme = href
        .split_once("://")
        .map(|(_, r)| r)
        .unwrap_or(href.as_str());
    let before_q = after_scheme.split(['?', '#']).next().unwrap_or("/");
    let path = before_q.find('/').map(|i| &before_q[i..]).unwrap_or("/");
    cx.vm().string(path)
}

fn loc_get_search(
    cx: &mut NativeCx<'_>,
    _this: JsValue,
    _args: &[JsValue],
) -> Result<JsValue, RunError> {
    let href = cx
        .host_as::<AnvilJsDomHost>()
        .map(|h| h.location_href.clone())
        .unwrap_or_default();
    let before_hash = href.split('#').next().unwrap_or("");
    let search = before_hash
        .split_once('?')
        .map(|(_, q)| format!("?{q}"))
        .unwrap_or_default();
    cx.vm().string(&search)
}

fn loc_get_hash(
    cx: &mut NativeCx<'_>,
    _this: JsValue,
    _args: &[JsValue],
) -> Result<JsValue, RunError> {
    let href = cx
        .host_as::<AnvilJsDomHost>()
        .map(|h| h.location_href.clone())
        .unwrap_or_default();
    let hash = href
        .split_once('#')
        .map(|(_, h)| format!("#{h}"))
        .unwrap_or_default();
    cx.vm().string(&hash)
}

fn hist_push_state(
    cx: &mut NativeCx<'_>,
    _this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    if args.len() >= 3 && !args[2].is_nullish() {
        let next = arg_str(cx, args, 2)?;
        if !next.trim().is_empty()
            && let Some(h) = cx.host_as::<AnvilJsDomHost>()
        {
            h.location_href = crate::resolve_script_fetch_url(&h.location_href, &next);
        }
    }
    Ok(JsValue::UNDEFINED)
}

fn custom_elements_define(
    cx: &mut NativeCx<'_>,
    _this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let name = arg_str(cx, args, 0)?;
    if let Some(h) = cx.host_as::<AnvilJsDomHost>() {
        h.doc.define_custom_element(&name);
    }
    Ok(JsValue::UNDEFINED)
}

fn custom_elements_get(
    cx: &mut NativeCx<'_>,
    _this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let name = arg_str(cx, args, 0)?;
    let defined = cx
        .host_as::<AnvilJsDomHost>()
        .is_some_and(|h| h.doc.is_custom_element_defined(&name));
    if defined {
        cx.vm().string(&name)
    } else {
        Ok(JsValue::UNDEFINED)
    }
}

fn window_match_media(
    cx: &mut NativeCx<'_>,
    _this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let q = arg_str(cx, args, 0)?;
    let mql = cx.vm().new_plain_object()?;
    let qs = cx.vm().string(&q)?;
    cx.vm().define_data(mql, "matches", JsValue::FALSE)?;
    cx.vm().define_data(mql, "media", qs)?;
    Ok(JsValue::object(mql))
}

fn window_get_computed_style(
    cx: &mut NativeCx<'_>,
    _this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let obj = cx.vm().new_plain_object()?;
    if let Some(nid) = args.first().copied().and_then(|v| unwrap_node(cx, v)) {
        let inline = cx
            .host_as::<AnvilJsDomHost>()
            .and_then(|h| h.doc.get(nid).data().attr("style").map(str::to_string))
            .unwrap_or_default();
        for part in inline.split(';') {
            if let Some((k, v)) = part.split_once(':') {
                let k_clean = k.trim().to_ascii_lowercase();
                let v_clean = v.trim().to_string();
                if !k_clean.is_empty() {
                    let vs = cx.vm().string(&v_clean)?;
                    cx.vm().define_data(obj, &k_clean, vs)?;
                    let camel = kebab_to_camel(&k_clean);
                    cx.vm().define_data(obj, &camel, vs)?;
                }
            }
        }
    }
    cx.vm()
        .define_method(obj, "getPropertyValue", computed_style_get_prop)?;
    Ok(JsValue::object(obj))
}

fn computed_style_get_prop(
    cx: &mut NativeCx<'_>,
    this: JsValue,
    args: &[JsValue],
) -> Result<JsValue, RunError> {
    let Some(obj) = this.as_object() else {
        return cx.vm().string("");
    };
    let name = arg_str(cx, args, 0)?;
    let key = cx
        .heap()
        .key_from_str(name.trim())
        .map_err(|e| RunError::internal(format!("{e:?}")))?;
    if let Ok(Some(prop)) = cx.heap().get_own(obj, key)
        && let Some(val) = prop.slot.data()
    {
        return Ok(val);
    }
    cx.vm().string("")
}

#[allow(dead_code)]
fn _keep_dom_imports_used(d: &Document) -> bool {
    let _ = parse_html;
    matches!(d.get(d.root()).data(), NodeData::Document)
}
