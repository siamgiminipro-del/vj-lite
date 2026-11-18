const state = {
  user: null,
  userCount: 0,
  maxUsers: 50,
  contests: [],
  activeContestId: null,
  contestData: null,
  activeProblemId: null,
  submissions: [],
  standings: null,
  selectedOjTab: 'codeforces',
  awaitingReturn: false,
  sse: null,
};

const $ = (id) => document.getElementById(id);

function esc(s) {
  return String(s ?? '')
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}

function toast(msg, type = 'info') {
  const box = $('toast-container');
  const el = document.createElement('div');
  el.className = `toast ${type}`;
  el.textContent = msg;
  box.appendChild(el);
  setTimeout(() => el.remove(), 4200);
}

const API_BASE =
  location.protocol === 'http:' || location.protocol === 'https:'
    ? ''
    : localStorage.getItem('vj_api_base') || 'https://vj-lite-server.onrender.com';

async function api(url, opts = {}) {
  const token = localStorage.getItem('vj_token');
  const headers = { 'Content-Type': 'application/json', ...(opts.headers || {}) };
  if (token) headers.Authorization = `Bearer ${token}`;

  const res = await fetch(`${API_BASE}${url}`, {
    headers,
    ...opts,
    body: opts.body && typeof opts.body !== 'string' ? JSON.stringify(opts.body) : opts.body,
  });
  const data = await res.json().catch(() => ({}));
  if (!res.ok) throw new Error(data.error || `HTTP ${res.status}`);
  if (data.token) localStorage.setItem('vj_token', data.token);
  return data;
}

function tauriInvoke(cmd, args = {}) {
  const fn = window.__TAURI__?.core?.invoke || window.__TAURI_INTERNALS__?.invoke;
  if (!fn) throw new Error('Tauri IPC not available');
  return fn(cmd, args);
}

function inTauriApp() {
  return Boolean(window.isTauri || window.__TAURI__?.core?.invoke || window.__TAURI_INTERNALS__?.invoke);
}

function uiLog(msg) {
  if (inTauriApp()) {
    try {
      tauriInvoke('ui_log', { msg: String(msg) }).catch(() => {});
    } catch (_) {}
  }
}

function fmtDur(sec) {
  const s = Math.max(0, Math.floor(sec));
  const h = String(Math.floor(s / 3600)).padStart(2, '0');
  const m = String(Math.floor((s % 3600) / 60)).padStart(2, '0');
  const r = String(s % 60).padStart(2, '0');
  return `${h}:${m}:${r}`;
}

function fmtDate(sec) {
  return new Date(sec * 1000).toLocaleString([], {
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  });
}

function showView(view) {
  if (!state.user && view !== 'login-gate') {
    view = 'login-gate';
  }
  $('app-topbar').classList.toggle('hidden', !state.user);
  $('view-login-gate').classList.toggle('hidden', view !== 'login-gate');
  $('view-hub').classList.toggle('hidden', view !== 'hub');
  $('view-coordinator').classList.toggle('hidden', view !== 'coordinator');
  $('view-contest').classList.toggle('hidden', view !== 'contest');

  $('topbar-contest-meta').classList.toggle('hidden', !state.user || view !== 'contest');
  $('btn-nav-hub').classList.toggle('hidden', !state.user);
  $('btn-nav-create').classList.toggle('hidden', !state.user);
}

function renderHeader() {
  $('capacity-badge').textContent = `${state.userCount} / ${state.maxUsers} Users`;
  const isCoord = state.user && (state.user.role === 'coordinator' || state.user.role === 'admin');
  $('btn-sync-all').classList.toggle('hidden', !isCoord);
  $('clar-ann-wrap').classList.toggle('hidden', !isCoord);

  const uc = $('user-controls');
  if (!state.user) {
    uc.innerHTML = `<span class="pill pill-indigo">Please sign in</span>`;
    return;
  }

  const handles = (state.user.oj_handles || [])
    .map((h) => `${h.platform.slice(0, 2).toUpperCase()} ✓`)
    .join(' • ');
  uc.innerHTML = `
    ${handles ? `<span class="pill pill-emerald">${esc(handles)}</span>` : ''}
    <span class="pill">${esc(state.user.display_name)}</span>
    <button id="btn-logout" class="btn btn-outline btn-sm" type="button">Logout</button>
  `;
  $('btn-logout').addEventListener('click', async () => {
    await api('/api/auth/logout', { method: 'POST' });
    localStorage.removeItem('vj_token');
    if (state.sse) state.sse.close();
    state.user = null;
    state.contests = [];
    state.activeContestId = null;
    renderHeader();
    showView('login-gate');
    toast('Signed out.');
  });
}

function updateTimer() {
  const c = state.contestData?.contest;
  if (!c) return ($('contest-timer-pill').textContent = '--:--:--');
  const now = Math.floor(Date.now() / 1000);
  if (now < c.start_time) $('contest-timer-pill').textContent = `Starts ${fmtDur(c.start_time - now)}`;
  else if (now <= c.end_time) $('contest-timer-pill').textContent = `⏱ ${fmtDur(c.end_time - now)}`;
  else $('contest-timer-pill').textContent = 'Archived / Ended';
}

function renderHubContests() {
  const tbody = $('hub-contests-tbody');
  tbody.innerHTML = '';

  if (state.contests.length === 0) {
    tbody.innerHTML = '<tr><td colspan="7" class="muted">No contests yet. Create one above!</td></tr>';
    return;
  }

  for (const c of state.contests) {
    const tr = document.createElement('tr');
    const stPill =
      c.status === 'running'
        ? '<span class="pill pill-emerald">RUNNING</span>'
        : c.status === 'upcoming'
        ? '<span class="pill pill-indigo">UPCOMING</span>'
        : '<span class="pill">ENDED</span>';

    const canEnterDirect = c.joined || c.status === 'ended';
    const actionLabel = c.status === 'ended' ? 'Upsolve / Open' : c.joined ? 'Enter Contest' : 'Join with Code';

    tr.innerHTML = `
      <td>
        <strong>${esc(c.title)}</strong>
        <div class="muted" style="font-size:0.75rem">By ${esc(c.creator_name || 'Coordinator')}${
      c.access_code ? ` • Code: <code>${esc(c.access_code)}</code>` : ''
    }</div>
      </td>
      <td>${stPill}</td>
      <td style="font-size:0.8rem">${esc(fmtDate(c.start_time))}<div class="muted" style="font-size:0.74rem">to ${esc(
      fmtDate(c.end_time)
    )}</div></td>
      <td>${c.problem_count}</td>
      <td>${c.participant_count}</td>
      <td>${c.submission_count ?? 0}</td>
      <td>
        <div class="row-gap">
          <button class="btn btn-sm btn-outline btn-details" type="button">Details</button>
          <button class="btn btn-sm ${canEnterDirect ? 'btn-primary' : 'btn-outline'} btn-enter" type="button">${actionLabel}</button>
        </div>
      </td>
    `;

    tr.querySelector('.btn-details').addEventListener('click', () => openContestDetailsModal(c));
    tr.querySelector('.btn-enter').addEventListener('click', async () => {
      if (canEnterDirect) {
        await openContest(c.id);
      } else {
        openContestDetailsModal(c);
      }
    });

    tbody.appendChild(tr);
  }
}

async function openContestDetailsModal(c) {
  $('cd-title').textContent = c.title;
  $('cd-meta').textContent = `${c.status.toUpperCase()} • ${fmtDate(c.start_time)} → ${fmtDate(
    c.end_time
  )} • Penalty: ${c.penalty_minutes}m • Participants: ${c.participant_count} • Submissions: ${
    c.submission_count ?? 0
  }`;
  $('cd-body').innerHTML = '<div class="muted">Loading contest details...</div>';
  $('modal-contest-details').classList.remove('hidden');

  if (!c.joined && c.status !== 'ended') {
    $('cd-body').innerHTML = `
      <div class="callout callout-indigo">
        ${
          c.status === 'upcoming'
            ? 'This upcoming contest requires an Access Code to register and enter.'
            : 'This running contest is protected by an Access Code. Enter the access code to view problems and participate.'
        }
      </div>
      <form id="cd-join-form" class="row-gap">
        <input id="cd-code-input" type="text" required placeholder="Enter Access Code for this contest" style="flex:1" />
        <button id="cd-join-btn" class="btn btn-primary" type="submit">Join Contest →</button>
      </form>
    `;
    setTimeout(() => $('cd-code-input')?.focus(), 20);
    $('cd-join-form').addEventListener('submit', async (ev) => {
      ev.preventDefault();
      try {
        const res = await api('/api/contests/join', {
          method: 'POST',
          body: { accessCode: $('cd-code-input').value.trim(), contestId: c.id },
        });
        $('modal-contest-details').classList.add('hidden');
        toast(`Joined ${res.title}!`, 'success');
        await loadContests();
        await openContest(res.contestId);
      } catch (err) {
        toast(err.message, 'error');
      }
    });
    return;
  }

  try {
    const [detail, standings] = await Promise.all([
      api(`/api/contests/${c.id}`),
      api(`/api/contests/${c.id}/standings`),
    ]);

    const probsHtml = detail.problems
      .map(
        (p) =>
          `<div class="row-between" style="padding:0.45rem 0.6rem;background:var(--canvas);border-radius:8px;border:1px solid var(--border)">
            <span><strong>${esc(p.alias)}.</strong> ${esc(p.title)}</span>
            <span class="pill">${esc(p.platform)} ${esc(p.external_id || '')}</span>
          </div>`
      )
      .join('');

    const topRows = standings.rows
      .slice(0, 5)
      .map(
        (r) =>
          `<tr><td>#${r.rank}</td><td>${esc(r.display_name)}</td><td><strong>${r.solved_count}</strong></td><td>${r.total_penalty}</td></tr>`
      )
      .join('');

    $('cd-body').innerHTML = `
      <div>
        <div class="muted" style="font-size:0.78rem;font-weight:700;margin-bottom:0.4rem">PROBLEMS (${detail.problems.length})</div>
        <div class="form-stack" style="gap:0.4rem">${probsHtml}</div>
      </div>
      <div>
        <div class="muted" style="font-size:0.78rem;font-weight:700;margin-bottom:0.4rem">TOP STANDINGS</div>
        <table class="data-table">
          <thead><tr><th>Rank</th><th>Participant</th><th>Solved</th><th>Penalty</th></tr></thead>
          <tbody>${topRows || '<tr><td colspan="4" class="muted">No ranked submissions yet.</td></tr>'}</tbody>
        </table>
      </div>
      <button id="cd-open-btn" class="btn btn-primary btn-block" type="button">
        ${c.status === 'ended' ? 'Open Contest & Upsolve →' : 'Enter Contest Workspace →'}
      </button>
    `;
    $('cd-open-btn').addEventListener('click', async () => {
      $('modal-contest-details').classList.add('hidden');
      await openContest(c.id);
    });
  } catch (err) {
    $('cd-body').innerHTML = `<div class="muted">${esc(err.message)}</div>`;
  }
}

function renderContestSelect() {
  const sel = $('contest-select');
  sel.innerHTML = '';
  state.contests.forEach((c) => {
    const opt = document.createElement('option');
    opt.value = c.id;
    opt.textContent = `${c.title} (${c.status.toUpperCase()})`;
    sel.appendChild(opt);
  });
  if (state.activeContestId) sel.value = String(state.activeContestId);
}

function renderProblems() {
  const probs = state.contestData?.problems || [];
  const ul = $('problem-list');
  const clarSel = $('clar-alias');
  ul.innerHTML = '';
  clarSel.innerHTML = '<option value="">General</option>';

  const acSet = new Set(
    state.submissions
      .filter((s) => state.user && s.user_id === state.user.id && s.verdict === 'AC')
      .map((s) => s.problem_id)
  );

  for (const p of probs) {
    const opt = document.createElement('option');
    opt.value = p.alias;
    opt.textContent = `${p.alias} — ${p.title}`;
    clarSel.appendChild(opt);

    const li = document.createElement('li');
    li.className = `problem-item ${p.id === state.activeProblemId ? 'active' : ''}`;
    li.innerHTML = `
      <div class="row-gap">
        <span class="alias-badge">${esc(p.alias)}</span>
        <div>
          <div style="font-size:0.86rem;font-weight:600">${esc(p.title)}</div>
          <div style="font-size:0.72rem;color:var(--text-muted)">${esc(p.platform)} ${esc(p.external_id || '')}</div>
        </div>
      </div>
      ${acSet.has(p.id) ? '<span class="pill pill-emerald">AC</span>' : ''}
    `;
    li.addEventListener('click', () => {
      state.activeProblemId = p.id;
      renderProblems();
      renderActiveProblem();
    });
    ul.appendChild(li);
  }
}

function renderActiveProblem() {
  const probs = state.contestData?.problems || [];
  const p = probs.find((x) => x.id === state.activeProblemId) || probs[0];
  if (!p) {
    const c = state.contestData?.contest;
    $('prob-alias').textContent = '—';
    $('prob-platform-pill').textContent = c?.status === 'upcoming' ? 'Upcoming' : 'Locked';
    $('prob-title').textContent =
      c?.status === 'upcoming' ? 'Contest Has Not Started Yet' : 'No Problems Available';
    $('prob-limits').textContent = c ? `Starts ${fmtDate(c.start_time)}` : '';
    $('prob-statement').textContent =
      c?.status === 'upcoming'
        ? 'You are registered for this contest. Problems will unlock automatically when the contest starts.'
        : 'No problems in this contest.';
    $('prob-samples').innerHTML = '';
    $('ext-actions').classList.add('hidden');
    $('local-actions').classList.add('hidden');
    return;
  }
  state.activeProblemId = p.id;

  $('prob-alias').textContent = p.alias;
  $('prob-platform-pill').textContent =
    p.source_type === 'external' ? `${p.platform} • ${p.external_id}` : 'Local Judge';
  $('prob-title').textContent = p.title;
  $('prob-limits').textContent = `${p.time_limit_ms} ms • ${p.memory_limit_mb} MB`;
  $('prob-statement').textContent = p.statement;

  const smp = $('prob-samples');
  smp.innerHTML = '';
  for (const tc of p.sampleTests || []) {
    const d1 = document.createElement('div');
    d1.className = 'sample-box';
    d1.innerHTML = `<div class="muted" style="font-size:0.72rem">INPUT</div><pre>${esc(tc.input)}</pre>`;
    const d2 = document.createElement('div');
    d2.className = 'sample-box';
    d2.innerHTML = `<div class="muted" style="font-size:0.72rem">OUTPUT</div><pre>${esc(tc.output)}</pre>`;
    smp.appendChild(d1);
    smp.appendChild(d2);
  }

  const isExt = p.source_type === 'external';
  $('ext-actions').classList.toggle('hidden', !isExt);
  $('local-actions').classList.toggle('hidden', isExt);
  if (isExt) {
    $('ext-bridge-title').textContent = inTauriApp()
      ? `${p.platform.toUpperCase()} • Local Background WebView Ready`
      : `${p.platform.toUpperCase()} • Direct Submit + API Sync`;
    $('btn-submit-local-alt').classList.toggle('hidden', !p.hasLocalTests);
  }
}

function renderSubmissions() {
  const tbody = $('subs-tbody');
  tbody.innerHTML = '';
  if (state.submissions.length === 0) {
    tbody.innerHTML = '<tr><td colspan="9" class="muted">No submissions yet.</td></tr>';
    return;
  }
  for (const s of state.submissions) {
    const tr = document.createElement('tr');
    const vClass = s.verdict === 'AC' ? 'pill-emerald' : s.verdict === 'PENDING' ? 'pill-indigo' : 'pill-rose';
    tr.innerHTML = `
      <td>#${s.id}</td>
      <td>${new Date(s.submitted_at * 1000).toLocaleTimeString()}</td>
      <td>${esc(s.username)}</td>
      <td><strong>${esc(s.problem_alias)}</strong> — ${esc(s.problem_title)}</td>
      <td>${esc(s.platform || 'local')}</td>
      <td>${esc(s.language)}</td>
      <td><span class="pill ${vClass}">${esc(s.verdict)}</span></td>
      <td>${s.time_ms ? `${s.time_ms} ms` : '-'}</td>
      <td>${s.code !== undefined ? '<button class="btn btn-sm btn-outline view-btn" type="button">View</button>' : '-'}</td>
    `;
    const vb = tr.querySelector('.view-btn');
    if (vb) {
      vb.addEventListener('click', () => {
        $('code-modal-title').textContent = `#${s.id} — ${s.problem_alias} (${s.verdict})`;
        $('code-modal-body').textContent = s.code || '(No local code backup)';
        $('modal-code').classList.remove('hidden');
      });
    }
    tbody.appendChild(tr);
  }
}

function renderStandings() {
  const st = state.standings;
  if (!st) return;
  $('standings-freeze-note').textContent = st.is_frozen ? 'Scoreboard is currently FROZEN.' : '';
  $('standings-thead').innerHTML = `
    <tr>
      <th>#</th><th>Participant</th><th>Solved</th><th>Penalty</th>
      ${st.problems.map((p) => `<th>${esc(p.alias)} (${esc(p.platform)})</th>`).join('')}
    </tr>
  `;
  const tbody = $('standings-tbody');
  tbody.innerHTML = '';
  if (st.rows.length === 0) {
    tbody.innerHTML = `<tr><td colspan="${4 + st.problems.length}" class="muted">No ranked submissions yet.</td></tr>`;
    return;
  }
  for (const r of st.rows) {
    const tr = document.createElement('tr');
    const cells = st.problems
      .map((p) => {
        const c = r.problems[p.problem_id];
        if (!c) return '<td>-</td>';
        if (c.solved) {
          const cls = c.first_to_solve ? 'cell-first' : 'cell-ac';
          const tries = c.wrong_attempts > 0 ? `+${c.wrong_attempts}` : '+';
          return `<td><span class="cell-box ${cls}">${tries}<small>${c.solve_time_minutes}m</small></span></td>`;
        }
        if (c.frozen) return `<td><span class="cell-box cell-frozen">? ${c.frozen_attempts}</span></td>`;
        if (c.wrong_attempts > 0) return `<td><span class="cell-box cell-wa">-${c.wrong_attempts}</span></td>`;
        return '<td>-</td>';
      })
      .join('');
    tr.innerHTML = `<td><strong>${r.rank}</strong></td><td><strong>${esc(r.display_name)}</strong><div class="muted" style="font-size:0.74rem">${esc(r.email)}</div></td><td><strong>${r.solved_count}</strong></td><td>${r.total_penalty}</td>${cells}`;
    tbody.appendChild(tr);
  }
}

function renderClarifications() {
  const list = $('clar-list');
  const items = state.contestData?.clarifications || [];
  list.innerHTML = items.length ? '' : '<p class="muted">No clarifications yet.</p>';
  for (const c of items) {
    const div = document.createElement('div');
    div.className = 'card';
    div.style.background = 'var(--canvas)';
    div.innerHTML = `
      <div class="row-between"><strong>${c.is_announcement ? '📢 Announcement' : esc(c.username)}</strong><span class="pill">${esc(c.problem_alias || 'General')}</span></div>
      <p class="pre-wrap" style="margin:0.4rem 0">${esc(c.question)}</p>
      ${c.answer ? `<div class="muted"><strong>Reply:</strong> ${esc(c.answer)}</div>` : ''}
    `;
    list.appendChild(div);
  }
}

async function refreshMe() {
  const data = await api('/api/auth/me');
  state.user = data.user;
  state.userCount = data.userCount;
  state.maxUsers = data.maxUsers;
  renderHeader();
}

async function loadContests() {
  if (!state.user) return;
  const data = await api('/api/contests?limit=10');
  state.contests = data.contests;
  renderContestSelect();
  renderHubContests();
}

async function openContest(contestId) {
  const meta = state.contests.find((x) => x.id === contestId);
  if (meta && !meta.joined && meta.status !== 'ended') {
    await openContestDetailsModal(meta);
    return;
  }
  try {
    const data = await api(`/api/contests/${contestId}`);
    state.contestData = data;
    state.activeContestId = contestId;
    renderContestSelect();
    $('contest-title').textContent = data.contest.title;
    $('contest-desc').textContent = data.contest.description || '';
    $('contest-status-pill').textContent = data.contest.status.toUpperCase();
    $('contest-rules-pill').textContent = `Penalty: ${data.contest.penalty_minutes}m`;
    $('contest-access-pill').classList.toggle('hidden', !data.contest.access_code);
    if (data.contest.access_code) {
      $('contest-access-pill').textContent = `Access Code: ${data.contest.access_code}`;
    }
    updateTimer();
    await Promise.all([loadSubmissions(contestId), loadStandings(contestId)]);
    renderProblems();
    renderActiveProblem();
    renderClarifications();
    showView('contest');
    connectSse(contestId);
  } catch (err) {
    showView('hub');
    if (meta && err.message && err.message.toLowerCase().includes('access code')) {
      await openContestDetailsModal(meta);
    } else {
      toast(err.message, 'error');
    }
  }
}

async function openCreateContestView() {
  const data = await api('/api/problems');
  const picker = $('cc-problem-list');
  picker.innerHTML = '';
  for (const p of data.problems) {
    const lbl = document.createElement('label');
    lbl.className = 'row-gap';
    lbl.style.flexDirection = 'row';
    lbl.innerHTML = `<input type="checkbox" value="${p.id}" /> [${esc(p.platform || 'local')}] ${esc(p.title)}`;
    picker.appendChild(lbl);
  }
  showView('coordinator');
}

function connectSse(contestId) {
  if (state.sse) state.sse.close();
  const es = new EventSource(`${API_BASE}/api/contests/${contestId}/events`);
  es.addEventListener('update', async () => {
    await Promise.all([loadSubmissions(contestId), loadStandings(contestId)]);
  });
  state.sse = es;
}

async function loadSubmissions(contestId) {
  const mine = $('filter-mine').checked ? '1' : '0';
  const data = await api(`/api/contests/${contestId}/submissions?mine=${mine}`);
  state.submissions = data.submissions;
  renderSubmissions();
  renderProblems();
}

async function loadStandings(contestId) {
  const data = await api(`/api/contests/${contestId}/standings`);
  state.standings = data;
  renderStandings();
}

async function handleExternalSubmit() {
  if (!state.user) return showView('login-gate');
  const prob = (state.contestData?.problems || []).find((p) => p.id === state.activeProblemId);
  if (!prob) return;
  const code = $('code-editor').value;
  const language = $('language-select').value;

  const prep = await api(`/api/contests/${state.activeContestId}/prepare-external-submit`, {
    method: 'POST',
    body: { problemId: prob.id, language, code },
  });

  if (inTauriApp()) {
    await tauriInvoke('submit_via_webview', {
      platform: prep.platform,
      submitUrl: prep.submitUrl,
      language,
      code,
    });
    toast(`Submitting to ${prep.platform} in background WebView...`, 'info');
  } else {
    if (code.trim() && navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(code).catch(() => {});
    }
    state.awaitingReturn = true;
    window.open(prep.submitUrl, '_blank', 'noopener,noreferrer');
    toast(`Opened ${prep.platform} submit tab — return here to auto-sync verdict.`, 'info');
  }
}

async function handleLocalJudge(useLocalAlt = false) {
  if (!state.user) return showView('login-gate');
  const code = $('code-editor').value;
  const language = $('language-select').value;
  if (!code.trim()) return toast('Write code first.', 'error');
  const res = await api(`/api/contests/${state.activeContestId}/submit`, {
    method: 'POST',
    body: { problemId: state.activeProblemId, language, code, useLocalJudge: useLocalAlt },
  });
  const s = res.submission;
  const box = $('last-verdict-box');
  box.classList.remove('hidden');
  box.innerHTML = `<strong>Verdict: ${esc(s.verdict)}</strong> • ${s.passed_tests}/${s.total_tests} tests • ${s.time_ms} ms ${
    s.error_message ? `<div style="margin-top:0.3rem">${esc(s.error_message)}</div>` : ''
  }`;
  toast(`Judged: ${s.verdict}`, s.verdict === 'AC' ? 'success' : 'error');
  await Promise.all([loadSubmissions(state.activeContestId), loadStandings(state.activeContestId)]);
}

async function completeLoginAndEnterHub(msg) {
  await refreshMe();
  await loadContests();
  showView('hub');
  if (msg) toast(msg, 'success');
}

function wireEvents() {
  document.querySelectorAll('.tab').forEach((b) => {
    b.addEventListener('click', () => {
      document.querySelectorAll('.tab').forEach((x) => x.classList.remove('active'));
      document.querySelectorAll('.tab-panel').forEach((x) => x.classList.remove('active'));
      b.classList.add('active');
      $(`tab-${b.dataset.tab}`).classList.add('active');
    });
  });

  $('brand-home').addEventListener('click', () => showView(state.user ? 'hub' : 'login-gate'));
  $('btn-nav-hub').addEventListener('click', () => showView('hub'));
  $('btn-back-hub').addEventListener('click', () => showView('hub'));
  $('btn-contest-back-hub').addEventListener('click', () => showView('hub'));
  $('btn-nav-create').addEventListener('click', () => openCreateContestView());
  $('btn-hub-create-contest').addEventListener('click', () => openCreateContestView());
  $('btn-refresh-hub').addEventListener('click', () => loadContests());

  $('btn-open-oj-bridge').addEventListener('click', () => $('modal-oj-bridge').classList.remove('hidden'));
  $('btn-close-oj-bridge').addEventListener('click', () => $('modal-oj-bridge').classList.add('hidden'));
  $('btn-close-cd').addEventListener('click', () => $('modal-contest-details').classList.add('hidden'));
  $('btn-close-code').addEventListener('click', () => $('modal-code').classList.add('hidden'));

  $('contest-select').addEventListener('change', (e) => openContest(Number(e.target.value)));

  // Google Auth (Tauri uses system browser + loopback; Web uses standard OAuth redirect)
  $('btn-google-signin').addEventListener('click', async () => {
    uiLog(`btn-google-signin clicked, inTauriApp=${inTauriApp()}, API_BASE=${API_BASE}`);
    const btn = $('btn-google-signin');
    const prevHtml = btn.innerHTML;
    btn.disabled = true;
    btn.textContent = 'Opening Google Sign-In...';
    try {
      const cfg = await api('/api/auth/config');
      uiLog(`auth/config: ${JSON.stringify(cfg)}`);
      if (cfg.supabaseEnabled && cfg.supabaseUrl) {
        if (inTauriApp()) {
          await tauriInvoke('start_supabase_google_oauth', {
            supabaseUrl: cfg.supabaseUrl,
          });
          toast('Opened default browser — complete Google Sign-In there.', 'info');
          return;
        }
        const redirectTo = window.location.origin;
        const authUrl = `${cfg.supabaseUrl.replace(/\/$/, '')}/auth/v1/authorize?provider=google&redirect_to=${encodeURIComponent(
          redirectTo
        )}`;
        window.location.href = authUrl;
        return;
      }

      const email = 'siam@gmail.com';
      await api('/api/auth/google', { method: 'POST', body: { email, displayName: 'siam' } });
      await completeLoginAndEnterHub(`Signed in with Google (${email})!`);
    } catch (e) {
      uiLog(`btn-google-signin error: ${e?.stack || e?.message || e}`);
      toast(e.message || 'Google Sign-In failed.', 'error');
    } finally {
      btn.disabled = false;
      btn.innerHTML = prevHtml;
    }
  });

  if (window.__TAURI__?.event?.listen) {
    window.__TAURI__.event.listen('supabase-oauth-token', async (event) => {
      try {
        const res = await api('/api/auth/supabase', {
          method: 'POST',
          body: { accessToken: event.payload },
        });
        await completeLoginAndEnterHub(`Signed in with Google (${res.user.email})!`);
      } catch (err) {
        toast(err.message, 'error');
      }
    });
  }

  $('btn-join-contest').addEventListener('click', async () => {
    try {
      const res = await api('/api/contests/join', {
        method: 'POST',
        body: { accessCode: $('join-access-code').value.trim() },
      });
      toast(`Joined ${res.title}!`, 'success');
      await loadContests();
      await openContest(res.contestId);
    } catch (e) {
      toast(e.message, 'error');
    }
  });

  document.querySelectorAll('.oj-tab-btn').forEach((b) => {
    b.addEventListener('click', () => {
      document.querySelectorAll('.oj-tab-btn').forEach((x) => x.classList.remove('btn-primary'));
      b.classList.add('btn-primary');
      state.selectedOjTab = b.dataset.oj;
      $('oj-handle-label').textContent = `${b.textContent} Handle`;
    });
  });

  $('oj-local-form').addEventListener('submit', async (e) => {
    e.preventDefault();
    try {
      const handle = $('oj-handle-input').value.trim();
      const password = $('oj-pass-input').value;
      if (inTauriApp() && password) {
        await tauriInvoke('save_oj_credentials', {
          platform: state.selectedOjTab,
          handle,
          password,
        });
      } else if (password) {
        localStorage.setItem(`vj_local_${state.selectedOjTab}`, JSON.stringify({ handle, hasLocalPass: true }));
      }
      await api('/api/oj/link', { method: 'POST', body: { platform: state.selectedOjTab, handle } });
      await refreshMe();
      $('modal-oj-bridge').classList.add('hidden');
      toast(`Linked ${state.selectedOjTab} handle @${handle} (credentials kept local).`, 'success');
    } catch (err) {
      toast(err.message, 'error');
    }
  });

  $('cp-platform').addEventListener('change', (e) => {
    const isLoc = e.target.value === 'local';
    $('cp-extid-wrap').classList.toggle('hidden', isLoc);
    $('cp-tests-wrap').classList.toggle('hidden', !isLoc);
    state.fetchedSamples = [];
  });

  $('btn-fetch-problem').addEventListener('click', async () => {
    const plat = $('cp-platform').value;
    const extId = $('cp-extid').value.trim();
    if (!extId) return toast('Enter a problem ID or URL first.', 'error');
    const btn = $('btn-fetch-problem');
    btn.disabled = true;
    btn.textContent = 'Fetching...';
    try {
      const res = await api('/api/problems/fetch', {
        method: 'POST',
        body: { platform: plat, externalId: extId },
      });
      const p = res.problem;
      $('cp-title').value = p.title || '';
      $('cp-statement').value = p.statement || '';
      state.fetchedSamples = p.sample_tests || [];
      toast(
        p.fetched_statement
          ? `Fetched "${p.title}" (${state.fetchedSamples.length} sample(s))!`
          : `Fetched title "${p.title}" (full HTML blocked by OJ, link added).`,
        'success'
      );
    } catch (err) {
      toast(err.message, 'error');
    } finally {
      btn.disabled = false;
      btn.textContent = '⚡ Fetch';
    }
  });

  $('create-problem-form').addEventListener('submit', async (e) => {
    e.preventDefault();
    try {
      const plat = $('cp-platform').value;
      const isLoc = plat === 'local';
      await api('/api/problems', {
        method: 'POST',
        body: {
          sourceType: isLoc ? 'local' : 'external',
          platform: plat,
          externalId: $('cp-extid').value.trim(),
          title: $('cp-title').value.trim() || null,
          statement: $('cp-statement').value.trim() || null,
          sampleTests: isLoc
            ? [{ input: $('cp-sin').value, output: $('cp-sout').value }]
            : state.fetchedSamples || [],
        },
      });
      state.fetchedSamples = [];
      $('create-problem-form').reset();
      toast('Problem saved!', 'success');
      await openCreateContestView();
    } catch (err) {
      toast(err.message, 'error');
    }
  });

  $('create-contest-form').addEventListener('submit', async (e) => {
    e.preventDefault();
    try {
      const problemIds = Array.from($('cc-problem-list').querySelectorAll('input:checked')).map((x) => Number(x.value));
      const now = Math.floor(Date.now() / 1000);
      const delaySec = Math.max(0, Number($('cc-delay')?.value || 0)) * 60;
      const startTime = now + delaySec;
      const res = await api('/api/contests', {
        method: 'POST',
        body: {
          title: $('cc-title').value.trim(),
          accessCode: $('cc-code').value.trim() || null,
          startTime,
          endTime: startTime + Number($('cc-duration').value || 120) * 60,
          penaltyMinutes: Number($('cc-penalty').value || 20),
          freezeMinutes: Number($('cc-freeze').value || 30),
          problemIds,
        },
      });
      navigator.clipboard?.writeText(res.access_code).catch(() => {});
      toast(`Contest created! Access Code: ${res.access_code}`, 'success');
      await loadContests();
      await openContest(res.id);
    } catch (err) {
      toast(err.message, 'error');
    }
  });

  $('btn-submit-webview').addEventListener('click', () => handleExternalSubmit().catch((e) => toast(e.message, 'error')));
  $('btn-submit-local-alt').addEventListener('click', () => handleLocalJudge(true).catch((e) => toast(e.message, 'error')));
  $('btn-submit-local').addEventListener('click', () => handleLocalJudge(false).catch((e) => toast(e.message, 'error')));
  $('btn-sync-verdicts').addEventListener('click', async () => {
    try {
      const r = await api(`/api/contests/${state.activeContestId}/sync`, { method: 'POST', body: {} });
      toast(`Synced ${r.syncedCount} new, ${r.updatedCount} updated.`, 'success');
      await Promise.all([loadSubmissions(state.activeContestId), loadStandings(state.activeContestId)]);
    } catch (e) {
      toast(e.message, 'error');
    }
  });
  $('btn-sync-all').addEventListener('click', async () => {
    try {
      const r = await api(`/api/contests/${state.activeContestId}/sync-all`, { method: 'POST', body: {} });
      toast(`Synced ${r.totalSynced} new across ${r.checkedHandles} handles.`, 'success');
      await Promise.all([loadSubmissions(state.activeContestId), loadStandings(state.activeContestId)]);
    } catch (e) {
      toast(e.message, 'error');
    }
  });

  $('filter-mine').addEventListener('change', () => loadSubmissions(state.activeContestId));
  $('btn-refresh-subs').addEventListener('click', () => loadSubmissions(state.activeContestId));
  $('btn-refresh-standings').addEventListener('click', () => loadStandings(state.activeContestId));

  $('clar-form').addEventListener('submit', async (e) => {
    e.preventDefault();
    try {
      await api(`/api/contests/${state.activeContestId}/clarifications`, {
        method: 'POST',
        body: {
          problemAlias: $('clar-alias').value,
          question: $('clar-question').value.trim(),
          isAnnouncement: $('clar-is-ann').checked,
        },
      });
      $('clar-question').value = '';
      await openContest(state.activeContestId);
      toast('Posted!', 'success');
    } catch (e) {
      toast(e.message, 'error');
    }
  });

  document.addEventListener('visibilitychange', () => {
    if (document.visibilityState === 'visible' && state.awaitingReturn) {
      state.awaitingReturn = false;
      $('btn-sync-verdicts').click();
    }
  });
}

async function handleSupabaseOAuthReturn() {
  if (!window.location.hash || !window.location.hash.includes('access_token=')) return;
  const params = new URLSearchParams(window.location.hash.slice(1));
  const accessToken = params.get('access_token');
  if (!accessToken) return;

  history.replaceState(null, '', window.location.pathname + window.location.search);
  try {
    const res = await api('/api/auth/supabase', {
      method: 'POST',
      body: { accessToken },
    });
    toast(`Signed in with Google (${res.user.email})!`, 'success');
  } catch (e) {
    toast(e.message, 'error');
  }
}

window.addEventListener('error', (ev) => uiLog(`window.error: ${ev.message} @ ${ev.filename}:${ev.lineno}`));
window.addEventListener('unhandledrejection', (ev) => uiLog(`unhandledrejection: ${ev.reason}`));

(async function init() {
  uiLog(`init start: href=${location.href}, protocol=${location.protocol}, API_BASE=${API_BASE}, hasGlobal=${Boolean(window.__TAURI__?.core?.invoke)}, hasInternals=${Boolean(window.__TAURI_INTERNALS__?.invoke)}`);
  wireEvents();
  uiLog('wireEvents complete');
  await handleSupabaseOAuthReturn();
  try {
    await refreshMe();
  } catch (e) {
    uiLog(`refreshMe error: ${e?.message || e}`);
  }
  if (state.user) {
    await loadContests();
    showView('hub');
  } else {
    showView('login-gate');
  }
  setInterval(updateTimer, 1000);
})();
