// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0-OR-Commercial
// Copyright (c) 2026 Buntato
// AnvilVM Built-in Async Microtask, URL Query, Storage & DOM Event Helper Prelude
// Author: Buntato

function anvilParseQueryParam(urlOrSearch, paramKey) {
  let raw = String(urlOrSearch);
  let qIndex = raw.indexOf('?');
  let queryPart = qIndex >= 0 ? raw.slice(qIndex + 1) : raw;
  let pairs = queryPart.split('&');
  for (let i = 0; i < pairs.length; i++) {
    let pair = pairs[i];
    let eqIdx = pair.indexOf('=');
    if (eqIdx > 0) {
      let k = pair.slice(0, eqIdx).trim();
      let v = pair.slice(eqIdx + 1).trim();
      if (k === paramKey) {
        return decodeURIComponent(v);
      }
    }
  }
  return '';
}

function anvilSetStorageJson(key, valueObj) {
  let raw = JSON.stringify(valueObj);
  localStorage.setItem(key, raw);
  return raw;
}

function anvilGetStorageJson(key) {
  let raw = localStorage.getItem(key);
  if (!raw) {
    return null;
  }
  return JSON.parse(raw);
}

function anvilToggleElementDisplay(el, visible, displayMode) {
  if (!el) {
    return false;
  }
  let mode = displayMode ? displayMode : 'block';
  if (visible) {
    el.style.display = mode;
  } else {
    el.style.display = 'none';
  }
  return visible;
}

function anvilQueryShowOnlyMatching(selector, attrName, activeValue) {
  let nodes = document.querySelectorAll(selector);
  let matchedCount = 0;
  nodes.forEach((node) => {
    let val = node.getAttribute(attrName);
    if (activeValue === 'all' || val === activeValue) {
      node.style.display = 'block';
      matchedCount++;
    } else {
      node.style.display = 'none';
    }
  });
  return matchedCount;
}

function anvilFilterCardsByText(selector, searchQuery) {
  let needle = anvilNormalizeQuery(searchQuery);
  let nodes = document.querySelectorAll(selector);
  let visibleCount = 0;
  nodes.forEach((node) => {
    let txt = String(node.textContent).toLowerCase();
    let attr = String(node.getAttribute('data-filter-tags')).toLowerCase();
    if (needle === '' || txt.includes(needle) || attr.includes(needle)) {
      node.style.display = 'block';
      visibleCount++;
    } else {
      node.style.display = 'none';
    }
  });
  return visibleCount;
}
