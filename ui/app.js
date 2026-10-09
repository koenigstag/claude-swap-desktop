'use strict';

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const POLL_VISIBLE_MS = 90_000;
const STEPS = [
  ['prepare', 'Read account'],
  ['backup', 'Save current default login'],
  ['login', 'Sign in (terminal)'],
  ['verify', 'Verify signed-in account'],
  ['save', 'claude-swap add'],
  ['restore', 'Switch default login back'],
  ['confirm', 'Confirm token status'],
];
const MARKS = { pending: '○', running: '◌', done: '✓', skipped: '–', warn: '!', failed: '✕' };

const $ = (id) => document.getElementById(id);
let state = null;
let busy = false;
let locked = loadPref('locked', true);
let pinned = false;
let pendingSwitch = null;
let reloginTarget = null;

/* ------------------------------ helpers ------------------------------- */

function loadPref(key, fallback) {
  try {
    const v = localStorage.getItem(key);
    return v === null ? fallback : JSON.parse(v);
  } catch {
    return fallback;
  }
}

function savePref(key, value) {
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch {
    /* storage unavailable — preference just won't stick */
  }
}

/** Tiny DOM builder: text is always set as textContent, never parsed as HTML. */
function h(tag, attrs = {}, ...children) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (v == null || v === false) continue;
    if (k === 'class') el.className = v;
    // CSSOM, not a style attribute: the CSP has no 'unsafe-inline' for styles.
    else if (k === 'style') el.style.cssText = v;
    else if (k.startsWith('on')) el.addEventListener(k.slice(2), v);
    else el.setAttribute(k, v === true ? '' : v);
  }
  for (const c of children.flat()) {
    if (c == null || c === false) continue;
    el.append(c instanceof Node ? c : document.createTextNode(String(c)));
  }
  return el;
}

const sameEmail = (a, b) => !!a && !!b && a.toLowerCase() === b.toLowerCase();

function primaryLine(tok) {
  if (!tok) return null;
  const want = tok.active ? 'active profile' : 'stored backup';
  return tok.lines.find((l) => l.source === want) || null;
}

/**
 * Credential health, matching the notifications: 'bad' when any copy lost its
 * refresh token (Claude Code wipes a copy only after a rejected refresh) or
 * the copy claude-swap relies on is missing; 'idle' when that copy's access
 * token expired but its refresh token renews it on next use; else 'ok'.
 */
function tokenState(tok) {
  if (tok.lines.some((l) => !l.refresh)) return 'bad';
  const l = primaryLine(tok);
  if (!l) return tok.active || tok.noCredentials ? 'bad' : 'idle';
  return l.state === 'fresh' ? 'ok' : 'idle';
}

function tokenBadge(tok) {
  if (!tok) return null;
  switch (tokenState(tok)) {
    case 'ok':
      return h('span', { class: 'badge ok' }, 'token ok');
    case 'idle':
      return h('span', { class: 'badge', title: 'The access token expired; its refresh token renews it on next use' }, 'token idle');
    default:
      return h('span', { class: 'badge bad', title: 'The refresh token is gone: sign in again with Re-login' }, 'needs login');
  }
}

const lineDot = (l) => (!l.refresh ? 'bad' : l.state === 'fresh' ? 'ok' : 'warn');

function ago(ms) {
  const s = Math.round((Date.now() - ms) / 1000);
  if (s < 60) return 'just now';
  if (s < 3600) return `${Math.round(s / 60)}m ago`;
  return `${Math.round(s / 3600)}h ago`;
}

/* ------------------------------- render -------------------------------- */

function render() {
  $('btn-lock').textContent = locked ? '🔒' : '🔓';
  $('btn-lock').title = locked ? 'Locked: switching is disabled. Click to unlock.' : 'Unlocked: click to lock switching.';
  $('btn-pin').classList.toggle('on', pinned);

  if (!state) return;
  const login = state.defaultLogin;
  $('default-login').textContent = login
    ? login.loggedIn
      ? `Default login: ${login.email}`
      : 'Default login: signed out'
    : 'Default login: unknown';

  renderWarnings();
  renderAccounts();
  renderMappings();
  $('updated').textContent = state.fetchedAt ? `Updated ${ago(state.fetchedAt)}` : '';
}

/**
 * Warnings start collapsed to their one-line title; these are the ones the
 * user expanded, by id. An id names the situation (and the account involved),
 * so an expanded warning stays open across refreshes.
 */
let expandedWarnings = new Set(loadPref('expandedWarnings', []));

function collectWarnings() {
  const out = [];
  const errorTitles = {
    listError: 'claude-swap failed',
    tokensError: 'Token status unavailable',
    defaultLoginError: "Couldn't read the default login",
  };
  for (const [key, title] of Object.entries(errorTitles)) {
    if (state[key]) out.push({ id: key, title, text: state[key], bad: true });
  }

  const accounts = state.list?.accounts || [];
  const active = accounts.find((a) => a.active);
  const login = state.defaultLogin;

  if (login && !login.loggedIn) {
    out.push({
      id: 'signed-out',
      title: 'Default login is signed out',
      text: 'The default login (~/.claude) is signed out. Re-login the active account or switch to another.',
      bad: true,
    });
  } else if (login && active && !sameEmail(login.email, active.email)) {
    out.push({
      id: `mismatch:${active.number}:${login.email}`,
      title: 'Default login changed outside claude-swap',
      text:
        `claude-swap thinks account ${active.number} (${active.email}) is the default login, ` +
        `but ~/.claude is signed in as ${login.email}.`,
      bad: true,
    });
  }

  if (active) {
    const mapped = (state.mappings || []).filter((m) => sameEmail(m.email, active.email));
    if (mapped.length) {
      out.push({
        id: `shared-token:${active.number}`,
        title: `Account ${active.number} shares its refresh token with mapped folders`,
        text:
          `Account ${active.number} is the default login and is also mapped to ${mapped.map((m) => m.displayPath || m.path).join(', ')}. ` +
          'Claude sessions in those folders run from a session profile with the same single-use refresh token, ' +
          'so one copy can invalidate the other. Prefer a different default login.',
      });
    }
  }
  return out;
}

function renderWarnings() {
  const warnings = collectWarnings();

  // Forget expanded ids for warnings that have cleared, so if the same
  // situation comes back later it starts collapsed again.
  const current = new Set(warnings.map((w) => w.id));
  const kept = [...expandedWarnings].filter((id) => current.has(id));
  if (kept.length !== expandedWarnings.size) {
    expandedWarnings = new Set(kept);
    savePref('expandedWarnings', kept);
  }

  $('warnings').replaceChildren(
    ...warnings.map((w) => {
      const item = h(
        'details',
        { class: `warning ${w.bad ? 'bad' : ''}`, open: expandedWarnings.has(w.id) },
        h('summary', {}, w.title),
        h('p', {}, w.text),
      );
      item.addEventListener('toggle', () => {
        if (item.open) expandedWarnings.add(w.id);
        else expandedWarnings.delete(w.id);
        savePref('expandedWarnings', [...expandedWarnings]);
      });
      return item;
    }),
  );
}

/* ------------------------------- tabs ---------------------------------- */

let activeTab = loadPref('tab', 'profiles');

function showTab(tab) {
  activeTab = tab === 'mappings' ? 'mappings' : 'profiles';
  savePref('tab', activeTab);
  for (const btn of document.querySelectorAll('.tabs [data-tab]')) {
    const on = btn.dataset.tab === activeTab;
    btn.classList.toggle('on', on);
    btn.setAttribute('aria-selected', String(on));
  }
  $('tab-profiles').hidden = activeTab !== 'profiles';
  $('tab-mappings').hidden = activeTab !== 'mappings';
}

/* Folder matching the way claude-swap does it: case-folded, and the most
   specific (longest) mapped ancestor-or-self wins. */
const normPath = (p) => (p || '').replace(/[\\/]+$/, '').toLowerCase();
const shownPath = (m) => m.displayPath || m.path;
const isInside = (child, parent) => {
  const c = normPath(child);
  const p = normPath(parent);
  return c === p || c.startsWith(p + '\\');
};

/** The mapping that applies to `path`, ignoring the mapping stored as `excludeKey`. */
function effectiveMapping(path, excludeKey) {
  return (
    (state.mappings || [])
      .filter((m) => m.path !== excludeKey && isInside(path, shownPath(m)))
      .sort((a, b) => normPath(shownPath(b)).length - normPath(shownPath(a)).length)[0] || null
  );
}

function accountByEmail(email) {
  return (state.list?.accounts || []).find((a) => sameEmail(a.email, email)) || null;
}

const accountLabel = (a) => (a ? `${a.number}. ${a.email}` : 'an unmanaged account');

function renderMappings() {
  const mappings = state.mappings || [];
  $('mappings-count').textContent = mappings.length ? String(mappings.length) : '';
  const root = $('mappings');
  if (!mappings.length) {
    root.replaceChildren(h('p', { class: 'muted center' }, 'No folder mappings.'));
    return;
  }
  const accounts = state.list?.accounts || [];
  const tokens = state.tokens || [];
  root.replaceChildren(
    ...mappings.map((m) => {
      const acct = accounts.find((a) => sameEmail(a.email, m.email));
      const tok = acct && tokens.find((t) => t.number === acct.number);
      return h(
        'div',
        { class: 'card mapping' },
        h('div', { class: 'path', title: m.displayPath || m.path }, m.displayPath || m.path),
        h(
          'div',
          { class: 'card-head' },
          h(
            'span',
            { class: 'muted who' },
            acct ? `→ ${acct.number}. ${acct.email}` : `→ ${m.email || 'unknown'}`,
          ),
          h(
            'div',
            { class: 'badges' },
            !m.exists && h('span', { class: 'badge bad' }, 'folder missing'),
            !acct && h('span', { class: 'badge bad' }, 'not managed'),
            acct?.active && h('span', { class: 'badge warn', title: 'Shares the default login’s refresh token' }, 'default login'),
            tokenBadge(tok),
          ),
        ),
        h(
          'div',
          { class: 'card-actions' },
          h('button', { onclick: () => openMap(m), disabled: busy || !m.exists, title: m.exists ? '' : 'The folder no longer exists' }, 'Change'),
          h('button', { onclick: () => openUnmap(m), disabled: busy }, 'Remove'),
        ),
      );
    }),
  );
}

/* --------------------------- add / change map --------------------------- */

let mapEdit = null; // { path, original: mapping | null }

function openMap(existing = null) {
  mapEdit = { path: existing ? shownPath(existing) : '', original: existing };
  $('map-title').textContent = existing ? 'Change mapping' : 'Add mapping';
  $('map-path').value = mapEdit.path;
  $('map-choose').hidden = !!existing;

  const select = $('map-account');
  const accounts = state.list?.accounts || [];
  const current = existing ? accountByEmail(existing.email) : null;
  select.replaceChildren(
    ...accounts.map((a) =>
      h(
        'option',
        { value: String(a.number), selected: current?.number === a.number },
        `${a.number}. ${a.email}${a.active ? ' (default login)' : ''}`,
      ),
    ),
  );
  if (!current) {
    // New mappings default to a non-default account: that's the safe pairing.
    const safe = accounts.find((a) => !a.active) || accounts[0];
    if (safe) select.value = String(safe.number);
  }
  $('map-error').hidden = true;
  updateMapNotes();
  $('sheet-map').hidden = false;
}

function updateMapNotes() {
  const notes = [];
  const path = mapEdit?.path;
  const number = Number($('map-account').value);
  const account = (state.list?.accounts || []).find((a) => a.number === number);
  const originalAccount = mapEdit?.original ? accountByEmail(mapEdit.original.email) : null;

  if (path) {
    const exact = (state.mappings || []).find((m) => normPath(shownPath(m)) === normPath(path));
    if (exact && !mapEdit.original) {
      notes.push({ warn: true, text: `Already mapped to ${accountLabel(accountByEmail(exact.email))}. Saving replaces it.` });
    }
    const parent = effectiveMapping(path, exact?.path);
    if (parent) {
      notes.push({
        text: `Overrides ${shownPath(parent)} (${accountLabel(accountByEmail(parent.email))}) for this folder and its subfolders.`,
      });
    }
    const children = (state.mappings || []).filter(
      (m) => normPath(shownPath(m)) !== normPath(path) && isInside(shownPath(m), path),
    );
    if (children.length) {
      notes.push({ text: `Subfolders with their own mapping keep it: ${children.map(shownPath).join(', ')}.` });
    }
  }
  if (account?.active) {
    notes.push({
      warn: true,
      text:
        `Account ${account.number} is the default login. Claude sessions in this folder would run from a ` +
        'session profile that shares its single-use refresh token, so one copy can invalidate the other.',
    });
  }

  $('map-notes').replaceChildren(...notes.map((n) => h('li', { class: n.warn ? 'warn' : '' }, n.text)));
  const unchanged = originalAccount && originalAccount.number === number;
  $('map-save').disabled = busy || !path || !account || unchanged;
}

async function chooseFolder() {
  try {
    const picked = await invoke('pick_folder');
    if (picked) {
      mapEdit.path = picked;
      $('map-path').value = picked;
      updateMapNotes();
    }
  } catch (e) {
    $('map-error').textContent = String(e);
    $('map-error').hidden = false;
  }
}

async function saveMap() {
  const number = Number($('map-account').value);
  busy = true;
  $('map-save').disabled = true;
  $('map-error').hidden = true;
  try {
    state.mappings = await invoke('map_folder', { number, path: mapEdit.path });
    $('sheet-map').hidden = true;
  } catch (e) {
    $('map-error').textContent = String(e);
    $('map-error').hidden = false;
  } finally {
    busy = false;
    render();
    updateMapNotes();
  }
}

/* ------------------------------ remove map ------------------------------ */

let unmapTarget = null;

function openUnmap(m) {
  unmapTarget = m;
  $('unmap-text').textContent = `${shownPath(m)} → ${accountLabel(accountByEmail(m.email))}`;
  const parent = effectiveMapping(shownPath(m), m.path);
  const active = (state.list?.accounts || []).find((a) => a.active);
  $('unmap-fallback').textContent = parent
    ? `New Claude sessions in this folder will use ${accountLabel(accountByEmail(parent.email))}, from the mapping of ${shownPath(parent)}.`
    : `New Claude sessions in this folder will use the default login${active ? ` (${accountLabel(active)})` : ''}. ` +
      'Sessions already running keep their account until restarted.';
  $('unmap-error').hidden = true;
  $('unmap-go').disabled = false;
  $('sheet-unmap').hidden = false;
}

async function doUnmap() {
  if (!unmapTarget) return;
  busy = true;
  $('unmap-go').disabled = true;
  $('unmap-error').hidden = true;
  try {
    state.mappings = await invoke('unmap_folder', { path: unmapTarget.path });
    $('sheet-unmap').hidden = true;
  } catch (e) {
    $('unmap-error').textContent = String(e);
    $('unmap-error').hidden = false;
    $('unmap-go').disabled = false;
  } finally {
    busy = false;
    render();
  }
}

function meter(label, window) {
  if (!window || typeof window.pct !== 'number') return null;
  const pct = Math.max(0, Math.min(100, window.pct));
  const level = pct >= 85 ? 'high' : pct >= 60 ? 'mid' : '';
  return h(
    'div',
    { class: 'meter', title: window.clock ? `Resets ${window.clock} (in ${window.countdown})` : '' },
    h('span', { class: 'muted' }, label),
    h('div', { class: 'bar' }, h('div', { class: `fill ${level}`, style: `width:${pct}%` })),
    h('span', { class: 'pct' }, `${Math.round(pct)}%`),
  );
}

function money(amount, currency = 'USD') {
  try {
    return new Intl.NumberFormat('en-US', { style: 'currency', currency }).format(amount);
  } catch {
    return `${amount.toFixed(2)} ${currency}`;
  }
}

/** A plan window (5h, 7d or a per-model one) is used up. */
function limitSpent(usage) {
  if (!usage) return false;
  return [usage.fiveHour, usage.sevenDay, ...(usage.scoped || [])].some(
    (w) => typeof w?.pct === 'number' && w.pct >= 100,
  );
}

/**
 * Extra-usage credits (claude-swap's `usage.spend`: used/limit in `currency`),
 * which pay for requests once a plan limit is used up.
 */
function creditsMeter(spend, highlight) {
  const hasLimit = typeof spend.limit === 'number' && spend.limit > 0;
  const rawPct = typeof spend.pct === 'number' ? spend.pct : hasLimit ? (spend.used / spend.limit) * 100 : 0;
  const pct = Math.max(0, Math.min(100, rawPct));
  const level = pct >= 85 ? 'high' : pct >= 60 ? 'mid' : '';
  const amount = hasLimit
    ? `${money(spend.used, spend.currency)} / ${money(spend.limit, spend.currency)}`
    : `${money(spend.used, spend.currency)} spent`;
  const title = [
    'Extra usage credits, used once a plan limit is reached',
    spend.clock && `resets ${spend.clock} (in ${spend.countdown})`,
  ]
    .filter(Boolean)
    .join(', ');
  return h(
    'div',
    { class: `meter money ${highlight ? 'on' : ''}`, title },
    h('span', { class: 'muted' }, 'Credits'),
    h('div', { class: 'bar' }, h('div', { class: `fill ${level}`, style: `width:${pct}%` })),
    h('span', { class: 'pct' }, amount),
  );
}

function renderAccounts() {
  const root = $('accounts');
  const accounts = state.list?.accounts;
  if (!accounts) {
    root.replaceChildren(h('p', { class: 'muted center' }, 'No account data from claude-swap.'));
    return;
  }
  const tokens = state.tokens || [];
  root.replaceChildren(
    ...accounts.map((a) => {
      const tok = tokens.find((t) => t.number === a.number);
      const usage = a.usage || a.lastGoodUsage;
      const stale = !a.usage && a.lastGoodUsage;
      const spent = limitSpent(usage);
      const spend = typeof usage?.spend?.used === 'number' ? usage.spend : null;
      // Credits matter once a limit is used up, or if some were spent already.
      const showCredits = spend && (spent || spend.used > 0);
      const creditsGone = spend && typeof spend.pct === 'number' && spend.pct >= 100;
      const limitBadge = !spent
        ? null
        : !spend
          ? h('span', { class: 'badge bad', title: 'A plan limit is used up and no extra usage credits are set up' }, 'limit reached')
          : creditsGone
            ? h('span', { class: 'badge bad', title: 'A plan limit and the extra usage credits are both used up' }, 'out of credits')
            : h('span', { class: 'badge warn', title: 'A plan limit is used up; requests are paid from extra usage credits' }, 'on credits');

      return h(
        'div',
        { class: `card ${a.active ? 'active' : ''}` },
        h(
          'div',
          { class: 'card-head' },
          h(
            'div',
            { class: 'who' },
            h('span', { class: 'email', title: a.email }, `${a.number}. ${a.email}`),
            h('span', { class: 'muted' }, a.organizationName || ''),
          ),
          h(
            'div',
            { class: 'badges' },
            a.active && h('span', { class: 'badge accent' }, 'default'),
            limitBadge,
            tokenBadge(tok),
          ),
        ),
        usage &&
          h(
            'div',
            { class: 'meters' },
            meter('5h', usage.fiveHour),
            meter('7d', usage.sevenDay),
            ...(usage.scoped || []).map((s) => meter(s.name, s)),
            showCredits && creditsMeter(spend, spent),
          ),
        stale && h('div', { class: 'meter-note' }, `Last known usage (${a.usageStatus})`),
        tok &&
          h(
            'ul',
            { class: 'tokens' },
            tok.lines.map((l) =>
              h(
                'li',
                {},
                h('span', { class: `dot ${lineDot(l)}` }),
                h('span', {}, `${l.source}: ${l.state}, refresh ${l.refresh ? 'yes' : 'no'}, expires ${l.expires}`),
              ),
            ),
            tok.noCredentials && tok.lines.length === 0 && h('li', {}, h('span', { class: 'dot bad' }), 'no credentials'),
          ),
        h(
          'div',
          { class: 'card-actions' },
          h('button', { onclick: () => openRelogin(a), disabled: busy }, 'Re-login'),
          !a.active &&
            h(
              'button',
              {
                onclick: () => openSwitch(a),
                disabled: busy || locked,
                title: locked ? 'Unlock (🔒) to switch' : '',
              },
              'Make default',
            ),
        ),
      );
    }),
  );
}

/* ------------------------------- data ---------------------------------- */

async function refresh() {
  $('btn-refresh').disabled = true;
  try {
    state = await invoke('get_state');
  } catch (e) {
    state = { ...(state || {}), listError: String(e) };
  } finally {
    $('btn-refresh').disabled = false;
  }
  render();
}

function sessionsText() {
  const n = (state?.sessions || []).length;
  if (!n) return '';
  return n === 1
    ? '1 Claude session is running on the default login. It will briefly see the other account.'
    : `${n} Claude sessions are running on the default login. They will briefly see the other account.`;
}

/* ------------------------------- switch -------------------------------- */

function openSwitch(account) {
  pendingSwitch = account;
  $('switch-text').textContent = `Make account ${account.number} (${account.email}) the default login in ~/.claude?`;
  const s = sessionsText();
  $('switch-sessions').textContent = s;
  $('switch-sessions').hidden = !s;
  $('sheet-switch').hidden = false;
}

async function doSwitch() {
  const account = pendingSwitch;
  $('sheet-switch').hidden = true;
  if (!account) return;
  busy = true;
  render();
  try {
    await invoke('switch_account', { number: account.number });
  } catch (e) {
    state = { ...state, listError: `Switch failed: ${e}` };
  } finally {
    busy = false;
    await refresh();
  }
}

/* ------------------------------ re-login ------------------------------- */

function openRelogin(account) {
  reloginTarget = account;
  const active = (state?.list?.accounts || []).find((a) => a.active);
  $('relogin-title').textContent = `Re-login account ${account.number}`;
  $('relogin-email').textContent = account.email;
  const restorable = active && active.number !== account.number;
  $('relogin-restore').checked = restorable;
  $('relogin-restore').disabled = !restorable;
  $('relogin-restore-label').textContent = restorable
    ? `Switch the default login back to account ${active.number} afterwards`
    : 'This account is already the default login';
  const s = sessionsText();
  $('relogin-sessions').textContent = s;
  $('relogin-sessions').hidden = !s;

  $('relogin-intro').hidden = false;
  $('relogin-progress').hidden = true;
  $('sheet-relogin').hidden = false;
}

function resetSteps() {
  $('relogin-steps').replaceChildren(
    ...STEPS.map(([id, label]) =>
      h(
        'li',
        { class: 'pending', 'data-step': id },
        h('span', { class: 'mark' }, MARKS.pending),
        h('span', { class: 'label' }, label),
        h('span', { class: 'detail' }),
      ),
    ),
  );
}

function onProgress({ step, status, detail }) {
  const li = $('relogin-steps').querySelector(`[data-step="${step}"]`);
  if (!li) return;
  li.className = status;
  li.querySelector('.mark').textContent = MARKS[status] || '•';
  li.querySelector('.detail').textContent = detail || '';
  // Cancelling only makes sense until the login window has closed.
  if (step === 'login' && status !== 'running') $('relogin-cancel').hidden = true;
}

async function startRelogin() {
  const account = reloginTarget;
  if (!account) return;
  $('relogin-intro').hidden = true;
  $('relogin-progress').hidden = false;
  $('relogin-result').hidden = true;
  $('relogin-done').hidden = true;
  $('relogin-cancel').hidden = false;
  resetSteps();

  busy = true;
  render();
  let outcome;
  try {
    outcome = await invoke('relogin', {
      number: account.number,
      restorePrevious: $('relogin-restore').checked,
    });
  } catch (e) {
    outcome = { ok: false, message: String(e) };
  }
  busy = false;

  const result = $('relogin-result');
  result.textContent = outcome.message;
  result.className = outcome.ok ? 'ok' : 'bad';
  result.hidden = false;
  $('relogin-cancel').hidden = true;
  $('relogin-done').hidden = false;
  refresh();
}

/* ------------------------------ autostart ------------------------------ */

async function loadAutostart() {
  const box = $('autostart');
  try {
    const { installed, enabled } = await invoke('get_autostart');
    box.checked = !!enabled;
    box.disabled = !installed;
    $('autostart-label').title = installed
      ? 'Start Claude Swap in the tray when you sign in to Windows'
      : 'Available in the installed app, not when running from a build folder';
  } catch {
    $('autostart-label').hidden = true;
  }
}

async function toggleAutostart(e) {
  const wanted = e.target.checked;
  try {
    e.target.checked = await invoke('set_autostart', { enabled: wanted });
  } catch (err) {
    e.target.checked = !wanted;
    showSettingsStatus(`Start with Windows: ${err}`);
  }
}

/* ------------------------------- settings ------------------------------ */

function showSettingsStatus(text) {
  $('settings-status').textContent = text;
  $('settings-status').hidden = !text;
}

function applySettings(s) {
  for (const box of document.querySelectorAll('[data-setting]')) box.checked = !!s[box.dataset.setting];
  for (const box of document.querySelectorAll('#notify-types [data-setting]')) box.disabled = !s.notifications;
}

async function openSettings() {
  showSettingsStatus('');
  await loadAutostart();
  try {
    applySettings(await invoke('get_settings'));
  } catch (err) {
    showSettingsStatus(`Couldn't load settings: ${err}`);
  }
  $('sheet-settings').hidden = false;
}

async function changeSetting(e) {
  const key = e.target.dataset.setting;
  try {
    applySettings(await invoke('set_settings', { patch: { [key]: e.target.checked } }));
  } catch (err) {
    e.target.checked = !e.target.checked;
    showSettingsStatus(`Couldn't save: ${err}`);
  }
}

async function sendTestNotification() {
  try {
    await invoke('test_notification');
    showSettingsStatus(
      'Test notification sent. If none appeared, check Windows Settings → System → Notifications (and Do not disturb).',
    );
  } catch (err) {
    showSettingsStatus(`Couldn't send it: ${err}`);
  }
}

/* ------------------------------- wiring -------------------------------- */

for (const btn of document.querySelectorAll('.tabs [data-tab]')) {
  btn.addEventListener('click', () => showTab(btn.dataset.tab));
}
showTab(activeTab);

$('btn-refresh').addEventListener('click', refresh);
$('btn-close').addEventListener('click', () => invoke('hide_window'));
$('btn-quit').addEventListener('click', () => invoke('quit_app'));
$('autostart').addEventListener('change', toggleAutostart);
$('btn-settings').addEventListener('click', openSettings);
$('settings-done').addEventListener('click', () => ($('sheet-settings').hidden = true));
$('btn-test-notification').addEventListener('click', sendTestNotification);
for (const box of document.querySelectorAll('[data-setting]')) box.addEventListener('change', changeSetting);
$('btn-tui').addEventListener('click', () => invoke('open_tui').catch((e) => alert(e)));
$('btn-lock').addEventListener('click', () => {
  locked = !locked;
  savePref('locked', locked);
  render();
});
$('btn-pin').addEventListener('click', () => {
  pinned = !pinned;
  invoke('set_pinned', { pinned });
  render();
});

$('btn-map-add').addEventListener('click', () => openMap());
$('map-choose').addEventListener('click', chooseFolder);
$('map-account').addEventListener('change', updateMapNotes);
$('map-cancel').addEventListener('click', () => ($('sheet-map').hidden = true));
$('map-save').addEventListener('click', saveMap);
$('unmap-cancel').addEventListener('click', () => ($('sheet-unmap').hidden = true));
$('unmap-go').addEventListener('click', doUnmap);

$('switch-cancel').addEventListener('click', () => ($('sheet-switch').hidden = true));
$('switch-go').addEventListener('click', doSwitch);

$('relogin-close-intro').addEventListener('click', () => ($('sheet-relogin').hidden = true));
$('relogin-start').addEventListener('click', startRelogin);
$('relogin-cancel').addEventListener('click', () => invoke('cancel_relogin'));
$('relogin-done').addEventListener('click', () => ($('sheet-relogin').hidden = true));

document.addEventListener('keydown', (e) => {
  if (e.key !== 'Escape') return;
  if (!$('sheet-switch').hidden) $('sheet-switch').hidden = true;
  else if (!$('sheet-settings').hidden) $('sheet-settings').hidden = true;
  else if (!$('sheet-map').hidden && !busy) $('sheet-map').hidden = true;
  else if (!$('sheet-unmap').hidden && !busy) $('sheet-unmap').hidden = true;
  else if (!$('sheet-relogin').hidden && !busy) $('sheet-relogin').hidden = true;
});

listen('relogin-progress', (e) => onProgress(e.payload));
// The background check (every 5 min) pushes fresh state while we're hidden.
listen('state-updated', (e) => {
  if (busy) return;
  state = e.payload;
  render();
});
listen('popover-shown', () => {
  loadAutostart(); // may have changed outside the app (Task Manager → Startup apps)
  if (!busy && (!state || Date.now() - (state.fetchedAt || 0) > 30_000)) refresh();
});

setInterval(() => {
  if (!busy && document.visibilityState === 'visible') refresh();
}, POLL_VISIBLE_MS);

render();
refresh();
loadAutostart();
