/*
 * The agent runtime. Installed by wtm into every page a browser pane shows, at document start, in
 * an ISOLATED content world: a JavaScript scope that shares the page's DOM but none of its
 * globals. The page cannot see `__wtm`, cannot redefine the DOM prototypes this file calls, and
 * cannot reach the `wtm` message handler its replies travel on — which is what lets an agent
 * trust a snapshot of a page it did not write. See `browser.rs` for the Rust half.
 *
 * # Two directions, one channel
 *
 * Rust asks by evaluating `__wtm.dispatch(id, method, args)` in this world; the answer is posted
 * to `webkit.messageHandlers.wtm` as `{id, ok, value|error}`. Everything the page does on its own
 * — a click in comment mode, a chord the page would otherwise swallow — is posted as
 * `{event, …}` with no id. Every post is a JSON *string*, so the Rust side never has to walk an
 * NSDictionary.
 *
 * # Refs
 *
 * A snapshot numbers the elements it reports (`e1`, `e2`, …) and keeps them in a `Map` this world
 * owns. An action names one of those. The map is rebuilt by every snapshot rather than kept
 * consistent across DOM mutation, which is the same contract Playwright's snapshot tool makes and
 * the one that fails loudly: a stale ref is refused with "take a new snapshot", never resolved to
 * whatever now sits where the old element was.
 *
 * # Synthesized events are not trusted events
 *
 * There is no way for a WKWebView embedder to deliver an OS-level click, so `click` dispatches
 * the pointer/mouse sequence and then calls `element.click()`, whose activation behaviour does
 * navigate links and toggle checkboxes. Frameworks that listen for `input`/`click` behave; a
 * native `<select>` popup, a file chooser, or a `window.open` that needs user activation will not.
 * That limit is documented in the tool descriptions rather than papered over.
 *
 * Plain script, no dependencies, no build step: Rust embeds this file with `include_str!`.
 */
(() => {
  'use strict';
  if (globalThis.__wtm) return;

  const HANDLER = 'wtm';
  const MAX_SNAPSHOT_CHARS = 40000;
  const MAX_NODES = 6000;
  const NAME_CHARS = 120;
  const EXCERPT_CHARS = 200;

  const post = (message) => {
    try {
      window.webkit.messageHandlers[HANDLER].postMessage(JSON.stringify(message));
    } catch {
      /* Detached document. Nothing is listening. */
    }
  };

  // ── refs ───────────────────────────────────────────────────────────────────────────────

  let refs = new Map();
  let refOf = new WeakMap();
  let refSeq = 0;

  function refFor(element) {
    let ref = refOf.get(element);
    if (ref && refs.get(ref) === element) return ref;
    refSeq += 1;
    ref = `e${refSeq}`;
    refs.set(ref, element);
    refOf.set(element, ref);
    return ref;
  }

  function resolve(ref) {
    if (typeof ref !== 'string') throw new Error('a ref like "e12" is required');
    const element = refs.get(ref);
    if (!element || !element.isConnected) {
      throw new Error(`${ref} is not on the page any more; take a new snapshot`);
    }
    return element;
  }

  // ── reading the page ───────────────────────────────────────────────────────────────────

  const SKIP_TAGS = new Set([
    'SCRIPT',
    'STYLE',
    'NOSCRIPT',
    'TEMPLATE',
    'HEAD',
    'META',
    'LINK',
    'TITLE',
  ]);
  const LANDMARKS = {
    NAV: 'navigation',
    MAIN: 'main',
    HEADER: 'banner',
    FOOTER: 'contentinfo',
    ASIDE: 'complementary',
    FORM: 'form',
    SECTION: 'region',
    ARTICLE: 'article',
    DIALOG: 'dialog',
    TABLE: 'table',
    THEAD: 'rowgroup',
    TBODY: 'rowgroup',
    TR: 'row',
    TH: 'columnheader',
    TD: 'cell',
    UL: 'list',
    OL: 'list',
    LI: 'listitem',
    DETAILS: 'group',
    SUMMARY: 'button',
    FIELDSET: 'group',
    IMG: 'img',
    SVG: 'img',
    VIDEO: 'video',
    AUDIO: 'audio',
    IFRAME: 'iframe',
    P: 'paragraph',
    BLOCKQUOTE: 'blockquote',
    PRE: 'code',
    CODE: 'code',
    HR: 'separator',
    LABEL: 'label',
  };
  const TEXT_INPUTS = new Set([
    'text',
    'search',
    'email',
    'url',
    'tel',
    'number',
    'password',
    'date',
    'time',
    'datetime-local',
    'month',
    'week',
    'color',
    'range',
  ]);

  function isHidden(element) {
    if (element.hidden || element.getAttribute('aria-hidden') === 'true') return true;
    const style = getComputedStyle(element);
    if (style.display === 'none' || style.visibility === 'hidden') return true;
    if (element.tagName === 'INPUT' && element.type === 'hidden') return true;
    return false;
  }

  function collapse(text, limit) {
    const compact = (text || '').replace(/\s+/g, ' ').trim();
    return compact.length > limit ? `${compact.slice(0, limit - 1)}…` : compact;
  }

  function roleOf(element) {
    const explicit = element.getAttribute('role');
    if (explicit) return explicit.split(/\s+/)[0];
    const tag = element.tagName;
    if (tag === 'A') return element.hasAttribute('href') ? 'link' : 'generic';
    if (tag === 'BUTTON') return 'button';
    if (tag === 'INPUT') {
      const type = (element.type || 'text').toLowerCase();
      if (type === 'button' || type === 'submit' || type === 'reset' || type === 'image')
        return 'button';
      if (type === 'checkbox') return 'checkbox';
      if (type === 'radio') return 'radio';
      if (type === 'file') return 'button';
      if (type === 'range') return 'slider';
      return 'textbox';
    }
    if (tag === 'TEXTAREA') return 'textbox';
    if (tag === 'SELECT') return element.multiple ? 'listbox' : 'combobox';
    if (tag === 'OPTION') return 'option';
    if (/^H[1-6]$/.test(tag)) return 'heading';
    if (element.isContentEditable && element.getAttribute('contenteditable') !== null)
      return 'textbox';
    return LANDMARKS[tag] || 'generic';
  }

  const INTERACTIVE = new Set([
    'link',
    'button',
    'checkbox',
    'radio',
    'textbox',
    'combobox',
    'listbox',
    'option',
    'slider',
    'tab',
    'menuitem',
    'menuitemcheckbox',
    'menuitemradio',
    'switch',
    'searchbox',
    'spinbutton',
    'treeitem',
  ]);

  function labelText(element) {
    const labelled = element.getAttribute('aria-labelledby');
    if (labelled) {
      const parts = labelled
        .split(/\s+/)
        .map((id) => document.getElementById(id))
        .filter(Boolean)
        .map((node) => node.textContent);
      if (parts.length) return parts.join(' ');
    }
    const aria = element.getAttribute('aria-label');
    if (aria) return aria;
    if (element.labels && element.labels.length) return element.labels[0].textContent;
    return null;
  }

  function nameOf(element, role) {
    const label = labelText(element);
    if (label) return collapse(label, NAME_CHARS);
    if (element.tagName === 'IMG' || element.tagName === 'SVG') {
      return collapse(
        element.getAttribute('alt') ||
          element.getAttribute('title') ||
          element.querySelector?.('title')?.textContent ||
          '',
        NAME_CHARS,
      );
    }
    if (element.tagName === 'INPUT') {
      const type = (element.type || 'text').toLowerCase();
      if (type === 'button' || type === 'submit' || type === 'reset')
        return collapse(element.value || type, NAME_CHARS);
      return collapse(
        element.placeholder || element.title || element.name || '',
        NAME_CHARS,
      );
    }
    if (element.tagName === 'TEXTAREA')
      return collapse(
        element.placeholder || element.title || element.name || '',
        NAME_CHARS,
      );
    if (element.tagName === 'SELECT')
      return collapse(element.title || element.name || '', NAME_CHARS);
    if (role === 'iframe')
      return collapse(
        element.title || element.getAttribute('name') || element.src || '',
        NAME_CHARS,
      );
    if (element.title && (role === 'generic' || role === 'img'))
      return collapse(element.title, NAME_CHARS);
    return collapse(element.innerText ?? element.textContent, NAME_CHARS);
  }

  function statesOf(element, role) {
    const out = [];
    if (element.disabled || element.getAttribute('aria-disabled') === 'true')
      out.push('disabled');
    if (
      role === 'checkbox' ||
      role === 'radio' ||
      role === 'switch' ||
      role === 'menuitemcheckbox'
    ) {
      const checked = element.checked ?? element.getAttribute('aria-checked') === 'true';
      out.push(checked ? 'checked' : 'unchecked');
    }
    const expanded = element.getAttribute('aria-expanded');
    if (expanded !== null) out.push(expanded === 'true' ? 'expanded' : 'collapsed');
    const pressed = element.getAttribute('aria-pressed');
    if (pressed !== null) out.push(pressed === 'true' ? 'pressed' : 'unpressed');
    const selected = element.getAttribute('aria-selected');
    if (selected === 'true' || element.selected === true) out.push('selected');
    if (element.required || element.getAttribute('aria-required') === 'true')
      out.push('required');
    if (element.readOnly) out.push('readonly');
    if (element.tagName === 'DETAILS') out.push(element.open ? 'open' : 'closed');
    if (role === 'heading') {
      const level = element.getAttribute('aria-level') || element.tagName.slice(1);
      out.push(`level=${level}`);
    }
    if (
      role === 'textbox' ||
      role === 'combobox' ||
      role === 'slider' ||
      role === 'searchbox'
    ) {
      let value = element.value;
      if (element.isContentEditable && value === undefined) value = element.innerText;
      if (element.type === 'password')
        value = value ? '•'.repeat(Math.min(value.length, 12)) : '';
      if (value !== undefined && value !== null && value !== '')
        out.push(`value=${JSON.stringify(collapse(String(value), NAME_CHARS))}`);
      if (element.tagName === 'SELECT') {
        const chosen = element.selectedOptions?.[0]?.textContent;
        if (chosen) out.push(`selected=${JSON.stringify(collapse(chosen, NAME_CHARS))}`);
      }
    }
    if (role === 'link') {
      const href = element.getAttribute('href');
      if (href && !href.startsWith('javascript:'))
        out.push(`href=${JSON.stringify(collapse(element.href, 200))}`);
    }
    if (element.tagName === 'IMG' && element.src)
      out.push(`src=${JSON.stringify(collapse(element.currentSrc || element.src, 120))}`);
    return out;
  }

  function snapshot({
    scope,
    interactiveOnly = false,
    maxChars = MAX_SNAPSHOT_CHARS,
  } = {}) {
    refs = new Map();
    refOf = new WeakMap();
    refSeq = 0;
    const root = scope ? resolve(scope) : document.body || document.documentElement;
    const lines = [];
    let chars = 0;
    let nodes = 0;
    let truncated = 0;

    const emit = (depth, text) => {
      if (chars + text.length + 1 > maxChars) {
        truncated += 1;
        return false;
      }
      lines.push(`${'  '.repeat(depth)}${text}`);
      chars += text.length + 1;
      return true;
    };

    const walk = (element, depth) => {
      nodes += 1;
      if (nodes > MAX_NODES) {
        truncated += 1;
        return;
      }
      if (SKIP_TAGS.has(element.tagName) || isHidden(element)) return;
      const role = roleOf(element);
      const interactive = INTERACTIVE.has(role);
      const landmark = role !== 'generic' && role !== 'paragraph' && role !== 'label';
      const own = ownText(element);
      let childDepth = depth;

      if (interactive || role === 'heading' || (!interactiveOnly && (landmark || own))) {
        const name = nameOf(element, role);
        const states = statesOf(element, role);
        const parts = [`- ${role}`];
        if (name) parts.push(JSON.stringify(name));
        parts.push(`[ref=${refFor(element)}]`);
        if (states.length) parts.push(states.join(' '));
        if (!emit(depth, parts.join(' '))) return;
        childDepth = depth + 1;
        // A control's text is its name, already reported; descending would repeat it.
        if (interactive && element.tagName !== 'SELECT') return;
        if (
          role === 'heading' ||
          role === 'paragraph' ||
          role === 'code' ||
          role === 'label'
        )
          return;
      } else if (!interactiveOnly && own && depth > 0) {
        emit(depth, `- text ${JSON.stringify(own)}`);
      }

      for (const child of element.children) walk(child, childDepth);
      if (element.shadowRoot)
        for (const child of element.shadowRoot.children) walk(child, childDepth);
    };

    walk(root, 0);
    // No header: Rust prefixes the webview's own URL and title, which are not the page's to write.
    let body = lines.join('\n');
    if (truncated > 0)
      body += `\n… truncated: ${truncated} more node${truncated === 1 ? '' : 's'}; snapshot a smaller scope with a ref, or raise max_chars`;
    return body;
  }

  /** Text this element has that is not inside a child element — what a wrapper "says" itself. */
  function ownText(element) {
    let text = '';
    for (const node of element.childNodes) {
      if (node.nodeType === Node.TEXT_NODE) text += node.data;
    }
    return collapse(text, NAME_CHARS);
  }

  function pageInfo() {
    return {
      url: location.href,
      title: document.title,
      readyState: document.readyState,
      viewport: { width: innerWidth, height: innerHeight },
      scroll: {
        x: scrollX,
        y: scrollY,
        width: document.documentElement.scrollWidth,
        height: document.documentElement.scrollHeight,
      },
    };
  }

  function getContent({ ref, format = 'text', maxChars = 60000 } = {}) {
    const root = ref ? resolve(ref) : document.body || document.documentElement;
    let text;
    if (format === 'html') text = root.outerHTML;
    else if (format === 'markdown') text = markdownOf(root);
    else text = root.innerText ?? root.textContent ?? '';
    const cut = text.length > maxChars;
    return `${text.slice(0, maxChars)}${cut ? `\n… truncated at ${maxChars} characters` : ''}`;
  }

  /** A small, honest Markdown serializer: headings, paragraphs, lists, links, code, emphasis. */
  function markdownOf(root) {
    const out = [];
    const inline = (node) => {
      if (node.nodeType === Node.TEXT_NODE) return node.data.replace(/\s+/g, ' ');
      if (
        node.nodeType !== Node.ELEMENT_NODE ||
        SKIP_TAGS.has(node.tagName) ||
        isHidden(node)
      )
        return '';
      const inner = Array.from(node.childNodes).map(inline).join('');
      switch (node.tagName) {
        case 'A':
          return node.href ? `[${inner.trim() || node.href}](${node.href})` : inner;
        case 'STRONG':
        case 'B':
          return `**${inner}**`;
        case 'EM':
        case 'I':
          return `*${inner}*`;
        case 'CODE':
          return `\`${inner}\``;
        case 'BR':
          return '\n';
        case 'IMG':
          return `![${node.alt || ''}](${node.currentSrc || node.src || ''})`;
        default:
          return inner;
      }
    };
    const block = (node, depth) => {
      if (
        node.nodeType !== Node.ELEMENT_NODE ||
        SKIP_TAGS.has(node.tagName) ||
        isHidden(node)
      )
        return;
      const tag = node.tagName;
      const heading = /^H([1-6])$/.exec(tag);
      if (heading) {
        out.push(`${'#'.repeat(Number(heading[1]))} ${inline(node).trim()}`, '');
        return;
      }
      if (tag === 'P' || tag === 'BLOCKQUOTE') {
        const text = inline(node).trim();
        if (text) out.push(tag === 'BLOCKQUOTE' ? `> ${text}` : text, '');
        return;
      }
      if (tag === 'PRE') {
        out.push('```', node.textContent.replace(/\n$/, ''), '```', '');
        return;
      }
      if (tag === 'UL' || tag === 'OL') {
        let index = 0;
        for (const item of node.children) {
          if (item.tagName !== 'LI') continue;
          index += 1;
          const marker = tag === 'OL' ? `${index}.` : '-';
          const text = Array.from(item.childNodes)
            .filter(
              (child) =>
                !(
                  child.nodeType === Node.ELEMENT_NODE &&
                  (child.tagName === 'UL' || child.tagName === 'OL')
                ),
            )
            .map(inline)
            .join('')
            .trim();
          out.push(`${'  '.repeat(depth)}${marker} ${text}`);
          for (const nested of item.children)
            if (nested.tagName === 'UL' || nested.tagName === 'OL')
              block(nested, depth + 1);
        }
        if (depth === 0) out.push('');
        return;
      }
      if (tag === 'TABLE') {
        const rows = Array.from(node.querySelectorAll('tr'));
        rows.forEach((row, i) => {
          const cells = Array.from(row.children).map((cell) =>
            inline(cell).trim().replace(/\|/g, '\\|'),
          );
          out.push(`| ${cells.join(' | ')} |`);
          if (i === 0) out.push(`| ${cells.map(() => '---').join(' | ')} |`);
        });
        out.push('');
        return;
      }
      if (tag === 'HR') {
        out.push('---', '');
        return;
      }
      const hasBlockChildren = Array.from(node.children).some((child) =>
        /^(H[1-6]|P|UL|OL|PRE|TABLE|DIV|SECTION|ARTICLE|MAIN|NAV|HEADER|FOOTER|ASIDE|BLOCKQUOTE|FORM|LI|HR|DETAILS|FIGURE)$/.test(
          child.tagName,
        ),
      );
      if (hasBlockChildren) {
        for (const child of node.children) block(child, depth);
      } else {
        const text = inline(node).trim();
        if (text) out.push(text, '');
      }
    };
    block(root, 0);
    return out
      .join('\n')
      .replace(/\n{3,}/g, '\n\n')
      .trim();
  }

  // ── acting on the page ─────────────────────────────────────────────────────────────────

  function centerOf(element) {
    const rect = element.getBoundingClientRect();
    return { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 };
  }

  function modifierInit(modifiers) {
    // `null` as well as absent: Rust passes JSON `null` for an argument the model left out.
    const list = Array.isArray(modifiers) ? modifiers : [];
    const set = new Set(list.map((m) => String(m).toLowerCase()));
    return {
      ctrlKey: set.has('control') || set.has('ctrl'),
      metaKey: set.has('meta') || set.has('cmd') || set.has('command'),
      shiftKey: set.has('shift'),
      altKey: set.has('alt') || set.has('option'),
    };
  }

  function fire(element, type, Ctor, init) {
    return element.dispatchEvent(
      new Ctor(type, {
        bubbles: true,
        cancelable: true,
        composed: true,
        view: window,
        ...init,
      }),
    );
  }

  function pointerSequence(element, { button = 0, modifiers, double = false } = {}) {
    element.scrollIntoView({ block: 'center', inline: 'center' });
    const { x, y } = centerOf(element);
    const base = {
      clientX: x,
      clientY: y,
      screenX: x,
      screenY: y,
      button,
      buttons: button === 2 ? 2 : 1,
      pointerId: 1,
      pointerType: 'mouse',
      isPrimary: true,
      ...modifierInit(modifiers),
    };
    fire(element, 'pointerover', PointerEvent, base);
    fire(element, 'mouseover', MouseEvent, base);
    fire(element, 'pointermove', PointerEvent, base);
    fire(element, 'mousemove', MouseEvent, base);
    fire(element, 'pointerdown', PointerEvent, base);
    const proceed = fire(element, 'mousedown', MouseEvent, base);
    if (proceed && typeof element.focus === 'function')
      element.focus({ preventScroll: true });
    fire(element, 'pointerup', PointerEvent, { ...base, buttons: 0 });
    fire(element, 'mouseup', MouseEvent, { ...base, buttons: 0 });
    return { base, proceed };
  }

  function click(args) {
    const element = resolve(args.ref);
    const { base } = pointerSequence(element, args);
    if (args.button === 2) {
      fire(element, 'contextmenu', MouseEvent, base);
    } else if (args.double) {
      element.click();
      fire(element, 'dblclick', MouseEvent, { ...base, detail: 2 });
    } else if (base.ctrlKey || base.metaKey || base.shiftKey || base.altKey) {
      // `element.click()` dispatches an unmodified click; a modified one has to be synthesized.
      fire(element, 'click', MouseEvent, { ...base, detail: 1 });
    } else {
      element.click();
    }
    return pageInfo();
  }

  function hover(args) {
    const element = resolve(args.ref);
    element.scrollIntoView({ block: 'center', inline: 'center' });
    const { x, y } = centerOf(element);
    const base = {
      clientX: x,
      clientY: y,
      pointerId: 1,
      pointerType: 'mouse',
      isPrimary: true,
    };
    fire(element, 'pointerover', PointerEvent, base);
    fire(element, 'pointerenter', PointerEvent, { ...base, bubbles: false });
    fire(element, 'mouseover', MouseEvent, base);
    fire(element, 'mouseenter', MouseEvent, { ...base, bubbles: false });
    fire(element, 'pointermove', PointerEvent, base);
    fire(element, 'mousemove', MouseEvent, base);
    return pageInfo();
  }

  /** Set a form control's value the way a framework expects: through the native setter, then `input`. */
  function setValue(element, value) {
    const proto =
      element instanceof HTMLTextAreaElement
        ? HTMLTextAreaElement.prototype
        : HTMLInputElement.prototype;
    const setter = Object.getOwnPropertyDescriptor(proto, 'value')?.set;
    if (setter) setter.call(element, value);
    else element.value = value;
    fire(element, 'input', InputEvent, { inputType: 'insertText', data: value });
  }

  function isTextControl(element) {
    if (element.tagName === 'TEXTAREA') return true;
    if (element.tagName === 'INPUT')
      return TEXT_INPUTS.has((element.type || 'text').toLowerCase());
    return element.isContentEditable;
  }

  function type(args) {
    const element = resolve(args.ref);
    const text = String(args.text ?? '');
    if (!isTextControl(element))
      throw new Error(`${args.ref} is a ${roleOf(element)}, not something to type into`);
    element.scrollIntoView({ block: 'center', inline: 'center' });
    element.focus({ preventScroll: true });
    if (
      element.isContentEditable &&
      element.tagName !== 'INPUT' &&
      element.tagName !== 'TEXTAREA'
    ) {
      if (args.clear) {
        const range = document.createRange();
        range.selectNodeContents(element);
        getSelection().removeAllRanges();
        getSelection().addRange(range);
        document.execCommand('delete');
      }
      document.execCommand('insertText', false, text);
    } else {
      const start = args.clear ? '' : element.value;
      setValue(element, start + text);
    }
    if (args.submit) submitFrom(element);
    else fire(element, 'change', Event, {});
    return pageInfo();
  }

  /** Enter pressed in a control: the page's own handler first, then the form's implicit submission. */
  function submitFrom(element) {
    const init = { key: 'Enter', code: 'Enter', keyCode: 13, which: 13 };
    const wanted = fire(element, 'keydown', KeyboardEvent, init);
    fire(element, 'keypress', KeyboardEvent, init);
    fire(element, 'change', Event, {});
    if (wanted && element.form && element.tagName !== 'TEXTAREA') {
      if (typeof element.form.requestSubmit === 'function') element.form.requestSubmit();
      else element.form.submit();
    }
    fire(element, 'keyup', KeyboardEvent, init);
  }

  const KEY_CODES = {
    Enter: 13,
    Tab: 9,
    Escape: 27,
    Backspace: 8,
    Delete: 46,
    ArrowUp: 38,
    ArrowDown: 40,
    ArrowLeft: 37,
    ArrowRight: 39,
    Home: 36,
    End: 35,
    PageUp: 33,
    PageDown: 34,
    ' ': 32,
    Space: 32,
  };

  function press(args) {
    const key = String(args.key || '');
    if (!key)
      throw new Error(
        'a key name is required, such as "Enter", "Escape", "ArrowDown" or "a"',
      );
    const target = args.ref ? resolve(args.ref) : document.activeElement || document.body;
    const name = key === 'Space' ? ' ' : key;
    const keyCode =
      KEY_CODES[key] ?? (name.length === 1 ? name.toUpperCase().charCodeAt(0) : 0);
    const init = {
      key: name,
      code: key.length === 1 ? `Key${key.toUpperCase()}` : key,
      keyCode,
      which: keyCode,
      ...modifierInit(args.modifiers),
    };
    if (name === 'Enter' && isTextControl(target) && !init.shiftKey) {
      submitFrom(target);
      return pageInfo();
    }
    const wanted = fire(target, 'keydown', KeyboardEvent, init);
    if (wanted) {
      if (name.length === 1 && isTextControl(target) && !init.metaKey && !init.ctrlKey) {
        fire(target, 'keypress', KeyboardEvent, init);
        if (
          target.isContentEditable &&
          target.tagName !== 'INPUT' &&
          target.tagName !== 'TEXTAREA'
        )
          document.execCommand('insertText', false, name);
        else setValue(target, target.value + name);
      } else if (
        name === 'Backspace' &&
        isTextControl(target) &&
        target.value !== undefined
      ) {
        setValue(target, target.value.slice(0, -1));
      } else if (name === 'Tab') {
        focusNext(init.shiftKey ? -1 : 1);
      } else if (
        name === 'Escape' &&
        document.activeElement &&
        document.activeElement !== document.body
      ) {
        document.activeElement.blur();
      }
    }
    fire(target, 'keyup', KeyboardEvent, init);
    return pageInfo();
  }

  function focusNext(direction) {
    const focusable = Array.from(
      document.querySelectorAll(
        'a[href], button, input, select, textarea, [tabindex]:not([tabindex="-1"]), [contenteditable="true"]',
      ),
    ).filter((el) => !el.disabled && !isHidden(el));
    const current = focusable.indexOf(document.activeElement);
    const next = focusable[(current + direction + focusable.length) % focusable.length];
    if (next) next.focus();
  }

  function selectOption(args) {
    const element = resolve(args.ref);
    if (element.tagName !== 'SELECT') throw new Error(`${args.ref} is not a <select>`);
    const wanted = Array.isArray(args.values)
      ? args.values.map(String)
      : [String(args.values ?? '')];
    let matched = 0;
    for (const option of element.options) {
      const hit =
        wanted.includes(option.value) || wanted.includes(option.textContent.trim());
      if (element.multiple) option.selected = hit;
      else if (hit) element.selectedIndex = option.index;
      if (hit) matched += 1;
    }
    if (matched === 0)
      throw new Error(
        `no option matched ${JSON.stringify(wanted)}; options are ${JSON.stringify(
          Array.from(element.options)
            .map((o) => o.textContent.trim())
            .slice(0, 40),
        )}`,
      );
    fire(element, 'input', Event, {});
    fire(element, 'change', Event, {});
    return pageInfo();
  }

  function scroll(args = {}) {
    if (args.toRef) {
      resolve(args.toRef).scrollIntoView({
        block: 'center',
        inline: 'center',
        behavior: 'instant',
      });
      return pageInfo();
    }
    const dx = Number(args.dx) || 0;
    const dy = Number(args.dy) || 0;
    if (args.ref) resolve(args.ref).scrollBy({ left: dx, top: dy, behavior: 'instant' });
    else window.scrollBy({ left: dx, top: dy, behavior: 'instant' });
    return pageInfo();
  }

  function fillForm(args) {
    const fields = Array.isArray(args.fields) ? args.fields : [];
    const done = [];
    for (const field of fields) {
      const element = resolve(field.ref);
      const role = roleOf(element);
      if (role === 'checkbox' || role === 'radio') {
        const want =
          field.value === true ||
          field.value === 'true' ||
          field.value === 'checked' ||
          field.value === 'on';
        if (element.checked !== want) element.click();
      } else if (element.tagName === 'SELECT') {
        selectOption({ ref: field.ref, values: field.value });
      } else {
        type({ ref: field.ref, text: String(field.value ?? ''), clear: true });
      }
      done.push(field.ref);
    }
    return { filled: done, ...pageInfo() };
  }

  function waitFor(args = {}) {
    const timeout = Math.min(Number(args.timeoutMs) || 10000, 60000);
    const check = () => {
      if (args.text && !(document.body?.innerText || '').includes(args.text)) return false;
      if (args.selector && !document.querySelector(args.selector)) return false;
      if (args.refGone) {
        const element = refs.get(args.refGone);
        if (element && element.isConnected && !isHidden(element)) return false;
      }
      if (args.load && document.readyState !== 'complete') return false;
      return true;
    };
    return new Promise((resolveWait, reject) => {
      if (check()) {
        resolveWait(pageInfo());
        return;
      }
      let settled = false;
      const finish = (ok) => {
        if (settled) return;
        settled = true;
        observer.disconnect();
        clearTimeout(timer);
        if (ok) resolveWait(pageInfo());
        else
          reject(
            new Error(`timed out after ${timeout} ms waiting for ${JSON.stringify(args)}`),
          );
      };
      const observer = new MutationObserver(() => {
        if (check()) finish(true);
      });
      observer.observe(document.documentElement, {
        childList: true,
        subtree: true,
        characterData: true,
        attributes: true,
      });
      if (args.load)
        window.addEventListener('load', () => check() && finish(true), { once: true });
      const timer = setTimeout(() => finish(check()), timeout);
    });
  }

  // ── overlay: highlights, pins, the comment popover ─────────────────────────────────────

  let host = null;
  let shadow = null;
  let theme = {
    accent: '#3b82f6',
    bg: '#111827',
    fg: '#f9fafb',
    border: '#374151',
    font: 'system-ui, sans-serif',
  };

  const OVERLAY_CSS = `
    :host { all: initial; position: fixed; inset: 0; pointer-events: none; z-index: 2147483647; font-family: var(--wtm-font); }
    .box { position: fixed; border: 2px solid var(--wtm-accent); border-radius: 3px; background: color-mix(in srgb, var(--wtm-accent) 12%, transparent); box-sizing: border-box; pointer-events: none; transition: opacity 120ms; }
    .box.flash { animation: wtm-flash 900ms ease-out forwards; }
    @keyframes wtm-flash { 0% { opacity: 1; } 70% { opacity: 1; } 100% { opacity: 0; } }
    .tag { position: absolute; top: -22px; left: -2px; padding: 2px 6px; font-size: 11px; line-height: 16px; color: var(--wtm-fg); background: var(--wtm-accent); border-radius: 3px 3px 3px 0; white-space: nowrap; max-width: 60vw; overflow: hidden; text-overflow: ellipsis; }
    .pin { position: fixed; width: 22px; height: 22px; border-radius: 11px 11px 11px 2px; background: var(--wtm-accent); color: var(--wtm-fg); font: 600 11px/22px var(--wtm-font); text-align: center; pointer-events: auto; cursor: pointer; box-shadow: 0 1px 4px rgba(0,0,0,.35); border: 1.5px solid var(--wtm-fg); }
    .pin.resolved { opacity: .5; }
    .pin.detached { border-style: dashed; opacity: .7; }
    .pop { position: fixed; width: 300px; padding: 10px; background: var(--wtm-bg); color: var(--wtm-fg); border: 1px solid var(--wtm-border); border-radius: 8px; box-shadow: 0 8px 24px rgba(0,0,0,.35); pointer-events: auto; font-size: 13px; display: flex; flex-direction: column; gap: 8px; }
    .pop .what { font-size: 11px; opacity: .75; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
    .pop textarea { all: initial; font: 13px/1.4 var(--wtm-font); color: var(--wtm-fg); background: transparent; border: 1px solid var(--wtm-border); border-radius: 6px; padding: 6px 8px; min-height: 60px; resize: vertical; box-sizing: border-box; width: 100%; }
    .pop textarea:focus { outline: 2px solid var(--wtm-accent); outline-offset: 1px; }
    .pop .row { display: flex; gap: 6px; justify-content: flex-end; }
    .pop button { all: initial; font: 500 12px/1 var(--wtm-font); padding: 6px 10px; border-radius: 6px; cursor: pointer; color: var(--wtm-fg); border: 1px solid var(--wtm-border); }
    .pop button.add { background: var(--wtm-accent); border-color: var(--wtm-accent); }
    .pop .hint { font-size: 11px; opacity: .6; }
  `;

  function ensureHost() {
    if (host && host.isConnected) return shadow;
    host = document.createElement('wtm-overlay');
    shadow = host.attachShadow({ mode: 'closed' });
    const style = document.createElement('style');
    style.textContent = OVERLAY_CSS;
    shadow.appendChild(style);
    applyTheme();
    (document.documentElement || document).appendChild(host);
    return shadow;
  }

  function applyTheme() {
    if (!host) return;
    host.style.setProperty('--wtm-accent', theme.accent);
    host.style.setProperty('--wtm-bg', theme.bg);
    host.style.setProperty('--wtm-fg', theme.fg);
    host.style.setProperty('--wtm-border', theme.border);
    host.style.setProperty('--wtm-font', theme.font);
  }

  function setTheme(tokens = {}) {
    theme = {
      ...theme,
      ...Object.fromEntries(
        Object.entries(tokens).filter(([, v]) => typeof v === 'string' && v),
      ),
    };
    applyTheme();
    return 'ok';
  }

  function placeBox(box, rect) {
    box.style.left = `${rect.left}px`;
    box.style.top = `${rect.top}px`;
    box.style.width = `${rect.width}px`;
    box.style.height = `${rect.height}px`;
  }

  function highlight(args) {
    const element = resolve(args.ref);
    element.scrollIntoView({ block: 'center', inline: 'center', behavior: 'instant' });
    const root = ensureHost();
    const box = document.createElement('div');
    box.className = 'box flash';
    placeBox(box, element.getBoundingClientRect());
    root.appendChild(box);
    setTimeout(() => box.remove(), 1000);
    return 'ok';
  }

  // ── comment mode ───────────────────────────────────────────────────────────────────────

  let commenting = false;
  let hoverBox = null;
  let popover = null;
  let pendingAnchor = null;
  let comments = [];
  let pinFrame = null;

  function cssPath(element) {
    if (element.id && document.querySelectorAll(`#${CSS.escape(element.id)}`).length === 1)
      return `#${CSS.escape(element.id)}`;
    const parts = [];
    let node = element;
    while (
      node &&
      node.nodeType === Node.ELEMENT_NODE &&
      node !== document.documentElement
    ) {
      let part = node.tagName.toLowerCase();
      if (node.id && document.querySelectorAll(`#${CSS.escape(node.id)}`).length === 1) {
        parts.unshift(`#${CSS.escape(node.id)}`);
        break;
      }
      const parent = node.parentElement;
      if (parent) {
        const siblings = Array.from(parent.children).filter(
          (child) => child.tagName === node.tagName,
        );
        if (siblings.length > 1) part += `:nth-of-type(${siblings.indexOf(node) + 1})`;
      }
      parts.unshift(part);
      node = parent;
    }
    const selector = parts.join(' > ');
    return selector || element.tagName.toLowerCase();
  }

  function headingFor(element) {
    let node = element;
    while (node && node !== document.body) {
      let sibling = node.previousElementSibling;
      while (sibling) {
        if (/^H[1-6]$/.test(sibling.tagName))
          return collapse(sibling.textContent, NAME_CHARS);
        const inner = sibling.querySelector?.('h1,h2,h3,h4,h5,h6');
        if (
          inner &&
          sibling.compareDocumentPosition(node) & Node.DOCUMENT_POSITION_FOLLOWING
        )
          return collapse(inner.textContent, NAME_CHARS);
        sibling = sibling.previousElementSibling;
      }
      node = node.parentElement;
    }
    return null;
  }

  function anchorOf(element) {
    const rect = element.getBoundingClientRect();
    const style = getComputedStyle(element);
    const role = roleOf(element);
    return {
      selector: cssPath(element),
      tag: element.tagName.toLowerCase(),
      role,
      name: nameOf(element, role),
      text: collapse(element.innerText ?? element.textContent, EXCERPT_CHARS),
      rect: {
        x: rect.left + scrollX,
        y: rect.top + scrollY,
        w: rect.width,
        h: rect.height,
      },
      url: location.href,
      pageTitle: document.title,
      nearestHeading: headingFor(element),
      styles: {
        color: style.color,
        background: style.backgroundColor,
        fontSize: style.fontSize,
        fontFamily: style.fontFamily,
        borderRadius: style.borderRadius,
      },
    };
  }

  function targetAt(x, y) {
    const element = document.elementFromPoint(x, y);
    if (
      !element ||
      element === host ||
      element === document.documentElement ||
      element === document.body
    )
      return null;
    return element;
  }

  function onCommentMove(event) {
    if (popover) return;
    const element = targetAt(event.clientX, event.clientY);
    const root = ensureHost();
    if (!hoverBox) {
      hoverBox = document.createElement('div');
      hoverBox.className = 'box';
      const tag = document.createElement('span');
      tag.className = 'tag';
      hoverBox.appendChild(tag);
      root.appendChild(hoverBox);
    }
    if (!element) {
      hoverBox.style.opacity = '0';
      return;
    }
    hoverBox.style.opacity = '1';
    placeBox(hoverBox, element.getBoundingClientRect());
    const role = roleOf(element);
    hoverBox.firstChild.textContent = `${element.tagName.toLowerCase()}${role !== 'generic' ? ` · ${role}` : ''}`;
  }

  function onCommentClick(event) {
    if (event.composedPath().includes(host)) return;
    event.preventDefault();
    event.stopPropagation();
    if (popover) {
      closePopover();
      return;
    }
    const element = targetAt(event.clientX, event.clientY);
    if (!element) return;
    pendingAnchor = anchorOf(element);
    if (hoverBox) hoverBox.style.opacity = '0';
    openPopover(element.getBoundingClientRect());
    post({ event: 'comment-pick', anchor: pendingAnchor });
  }

  function swallow(event) {
    if (event.composedPath().includes(host)) return;
    event.preventDefault();
    event.stopPropagation();
  }

  function openPopover(rect) {
    const root = ensureHost();
    popover = document.createElement('div');
    popover.className = 'pop';
    const what = document.createElement('div');
    what.className = 'what';
    what.textContent = `${pendingAnchor.tag}${pendingAnchor.name ? ` “${pendingAnchor.name}”` : ''}`;
    const textarea = document.createElement('textarea');
    textarea.placeholder = 'What should change here?';
    const row = document.createElement('div');
    row.className = 'row';
    const cancel = document.createElement('button');
    cancel.textContent = 'Cancel';
    cancel.addEventListener('click', () => closePopover());
    const add = document.createElement('button');
    add.className = 'add';
    add.textContent = 'Add comment';
    const submit = () => {
      const text = textarea.value.trim();
      if (!text) return;
      post({ event: 'comment-add', anchor: pendingAnchor, text });
      closePopover();
    };
    add.addEventListener('click', submit);
    textarea.addEventListener('keydown', (event) => {
      event.stopPropagation();
      if (event.key === 'Escape') closePopover();
      if (event.key === 'Enter' && (event.metaKey || event.ctrlKey)) submit();
    });
    const hint = document.createElement('div');
    hint.className = 'hint';
    hint.textContent = '⌘↩ to add · Esc to cancel';
    row.append(cancel, add);
    popover.append(what, textarea, row, hint);
    root.appendChild(popover);
    const left = Math.max(8, Math.min(innerWidth - 316, rect.left));
    const below = rect.bottom + 8;
    const top = below + 160 < innerHeight ? below : Math.max(8, rect.top - 168);
    popover.style.left = `${left}px`;
    popover.style.top = `${top}px`;
    textarea.focus();
  }

  function closePopover() {
    popover?.remove();
    popover = null;
    pendingAnchor = null;
  }

  function setCommentMode(on) {
    const enable = on === true || on === 'true';
    if (enable === commenting) return commenting;
    commenting = enable;
    if (enable) {
      ensureHost();
      document.addEventListener('mousemove', onCommentMove, true);
      document.addEventListener('click', onCommentClick, true);
      document.addEventListener('mousedown', swallow, true);
      document.addEventListener('mouseup', swallow, true);
      document.addEventListener('pointerdown', swallow, true);
      document.addEventListener('pointerup', swallow, true);
    } else {
      document.removeEventListener('mousemove', onCommentMove, true);
      document.removeEventListener('click', onCommentClick, true);
      document.removeEventListener('mousedown', swallow, true);
      document.removeEventListener('mouseup', swallow, true);
      document.removeEventListener('pointerdown', swallow, true);
      document.removeEventListener('pointerup', swallow, true);
      hoverBox?.remove();
      hoverBox = null;
      closePopover();
    }
    return commenting;
  }

  function renderPins(list) {
    comments = Array.isArray(list) ? list : [];
    drawPins();
    return comments.length;
  }

  function drawPins() {
    const root = ensureHost();
    for (const stale of root.querySelectorAll('.pin')) stale.remove();
    comments.forEach((comment, index) => {
      const anchor = comment.anchor || {};
      let element = null;
      try {
        element = anchor.selector ? document.querySelector(anchor.selector) : null;
      } catch {
        element = null;
      }
      const pin = document.createElement('div');
      pin.className = `pin${comment.status === 'resolved' ? ' resolved' : ''}${element ? '' : ' detached'}`;
      pin.textContent = String(index + 1);
      pin.title = comment.text || '';
      let left;
      let top;
      if (element) {
        const rect = element.getBoundingClientRect();
        left = rect.right - 11;
        top = rect.top - 11;
      } else if (anchor.rect) {
        left = anchor.rect.x + anchor.rect.w - scrollX - 11;
        top = anchor.rect.y - scrollY - 11;
      } else {
        return;
      }
      pin.style.left = `${Math.max(0, Math.min(innerWidth - 22, left))}px`;
      pin.style.top = `${Math.max(0, Math.min(innerHeight - 22, top))}px`;
      pin.addEventListener('click', (event) => {
        event.stopPropagation();
        post({ event: 'comment-pick', commentId: comment.id });
      });
      root.appendChild(pin);
    });
  }

  const schedulePins = () => {
    if (!comments.length || pinFrame !== null) return;
    pinFrame = requestAnimationFrame(() => {
      pinFrame = null;
      drawPins();
    });
  };
  addEventListener('scroll', schedulePins, { passive: true, capture: true });
  addEventListener('resize', schedulePins, { passive: true });

  // ── what the page does on its own ──────────────────────────────────────────────────────

  // A click in the page never reaches the app's own document, so the pane cannot know it was
  // chosen. Reported so the split target follows the pointer, as it does for every other pane.
  addEventListener('pointerdown', () => post({ event: 'focus' }), {
    capture: true,
    passive: true,
  });

  addEventListener(
    'keydown',
    (event) => {
      if (event.defaultPrevented) return;
      const meta = event.metaKey || event.ctrlKey;
      let action = null;
      if (meta && event.key === 'l') action = 'focus-address';
      else if (meta && event.key === '[') action = 'back';
      else if (meta && event.key === ']') action = 'forward';
      else if (meta && event.key === 'r') action = 'reload';
      else if (meta && event.key === 'j') action = 'shell';
      else if (meta && event.shiftKey && event.key.toLowerCase() === 'c')
        action = 'toggle-comments';
      else if (event.key === 'Escape' && commenting) action = 'escape';
      if (!action) return;
      event.preventDefault();
      event.stopPropagation();
      if (action === 'escape' && popover) {
        closePopover();
        return;
      }
      post({ event: 'shortcut', action });
    },
    true,
  );

  // ── dispatch ───────────────────────────────────────────────────────────────────────────

  const methods = {
    snapshot,
    pageInfo,
    getContent,
    click,
    hover,
    type,
    press,
    selectOption,
    scroll,
    fillForm,
    waitFor,
    highlight,
    setTheme,
    setCommentMode: (args) => setCommentMode(args.on),
    renderPins: (args) => renderPins(args.comments),
  };

  function dispatch(id, method, args) {
    const run = async () => {
      try {
        const fn = methods[method];
        if (!fn) throw new Error(`unknown method ${method}`);
        const value = await fn(args || {});
        post({ id, ok: true, value });
      } catch (error) {
        post({ id, ok: false, error: String((error && error.message) || error) });
      }
    };
    void run();
    return 'queued';
  }

  globalThis.__wtm = Object.freeze({ dispatch, version: 1 });
  post({ event: 'ready', url: location.href });
})();
