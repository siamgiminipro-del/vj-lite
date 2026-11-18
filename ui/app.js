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
  setTimeout(() => el.remove(), 4000);
}

const API_BASE =
  location.protocol === 'http:' || location.protocol === 'https:'
    ? ''
    : localStorage.getItem('vj_api_base') || 'http://localhost:3000';

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

function isTauri() {
  return Boolean(window.__TAURI__?.core?.invoke);
}

function fmtDur(sec) {
  const s = Math.max(0, Math.floor(sec));
  const h = String(Math.floor(s / 3600)).padStart(2, '0');
  const m = String(Math.floor((s % 3600) / 60)).padStart(2, '0');
  const r = String(s % 60).padStart(2, '0');
  return `${h}:${m}:${r}`;
}

function showView(view) {
  $('view-onboarding').classList.toggle('hidden', view !== 'onboarding');
  $('view-coordinator').classList.toggle('hidden', view !== 'coordinator');
  $('view-contest').classList.toggle('hidden', view !== 'contest');
}

function renderHeader() {
  $('capacity-badge').textContent = `${state.userCount} / ${state.maxUsers} Users`;
  const isCoord = state.user && (state.user.role === 'coordinator' || state.user.role === 'admin');
  $('btn-nav-coordinator').classList.toggle('hidden', !isCoord);
  $('btn-sync-all').classList.toggle('hidden', !isCoord);
  $('clar-ann-wrap').classList.toggle('hidden', !isCoord);

  const uc = $('user-controls');
  if (!state.user) {
    uc.innerHTML = `<button id="btn-go-login" class="btn btn-primary btn-sm" type="button">Sign In / Google</button>`;
    $('btn-go-login').addEventListener('click', () => showView('onboarding'));
    return;
  }

  const handles = (state.user.oj_handles || []).map((h) => `${h.platform.slice(0, 2).toUpperCase()} ✓`).join(' • ');
  uc.innerHTML = `
    ${handles ? `<span class="pill pill-emerald">${esc(handles)}</span>` : ''}
    <span class="pill">${esc(state.user.display_name)}</span>
    <button id="btn-logout" class="btn btn-outline btn-sm" type="button">Logout</button>
  `;
  $('btn-logout').addEventListener('click', async () => {
    await api('/api/auth/logout', { method: 'POST' });
    localStorage.removeItem('vj_token');
    state.user = null;
    await refreshMe();
    showView('onboarding');
    toast('Logged out.');
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

function renderContestList() {
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
    state.submissions.filter((s) => state.user && s.user_id === state.user.id && s.verdict === 'AC').map((s) => s.problem_id)
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
  if (!p) return;
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
    $('ext-bridge-title').textContent = isTauri()
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
  const data = await api('/api/contests?limit=10');
  state.contests = data.contests;
  if (!state.activeContestId && state.contests.length > 0) {
    state.activeContestId = state.contests[0].id;
  }
  renderContestList();
  if (state.activeContestId) {
    await openContest(state.activeContestId);
  }
}

async function openContest(contestId) {
  try {
    const data = await api(`/api/contests/${contestId}`);
    state.contestData = data;
    state.activeContestId = contestId;
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
    showView('onboarding');
    toast(err.message, 'error');
  }
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
  if (!state.user) return showView('onboarding');
  const prob = (state.contestData?.problems || []).find((p) => p.id === state.activeProblemId);
  if (!prob) return;
  const code = $('code-editor').value;
  const language = $('language-select').value;

  const prep = await api(`/api/contests/${state.activeContestId}/prepare-external-submit`, {
    method: 'POST',
    body: { problemId: prob.id, language, code },
  });

  if (isTauri()) {
    await window.__TAURI__.core.invoke('submit_via_webview', {
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
  if (!state.user) return showView('onboarding');
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

function wireEvents() {
  document.querySelectorAll('.tab').forEach((b) => {
    b.addEventListener('click', () => {
      document.querySelectorAll('.tab').forEach((x) => x.classList.remove('active'));
      document.querySelectorAll('.tab-panel').forEach((x) => x.classList.remove('active'));
      b.classList.add('active');
      $(`tab-${b.dataset.tab}`).classList.add('active');
    });
  });

  $('btn-nav-onboarding').addEventListener('click', () => showView('onboarding'));
  $('btn-back-contest').addEventListener('click', () => showView('contest'));
  $('btn-nav-coordinator').addEventListener('click', async () => {
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
  });

  $('contest-select').addEventListener('change', (e) => openContest(Number(e.target.value)));
  $('btn-close-code').addEventListener('click', () => $('modal-code').classList.add('hidden'));

  document.querySelectorAll('.demo-fill').forEach((b) => {
    b.addEventListener('click', () => {
      $('login-email').value = b.dataset.e;
      $('login-password').value = b.dataset.p;
    });
  });

  $('btn-google-signin').addEventListener('click', async () => {
    try {
      const cfg = await api('/api/auth/config');
      if (cfg.supabaseEnabled && cfg.supabaseUrl) {
        const redirectTo =
          location.protocol === 'http:' || location.protocol === 'https:'
            ? `${window.location.origin}/`
            : 'http://localhost:3000/';
        const authUrl = `${cfg.supabaseUrl.replace(/\/$/, '')}/auth/v1/authorize?provider=google&redirect_to=${encodeURIComponent(redirectTo)}`;
        window.location.href = authUrl;
        return;
      }

      const email = $('login-email').value.trim() || 'siam@gmail.com';
      await api('/api/auth/google', { method: 'POST', body: { email, displayName: email.split('@')[0] } });
      await refreshMe();
      await loadContests();
      toast(`Signed in with Google (${email})!`, 'success');
    } catch (e) { toast(e.message, 'error'); }
  });

  $('btn-email-login').addEventListener('click', async () => {
    try {
      await api('/api/auth/login', {
        method: 'POST',
        body: { email: $('login-email').value, password: $('login-password').value },
      });
      await refreshMe();
      await loadContests();
      toast('Signed in!', 'success');
    } catch (e) { toast(e.message, 'error'); }
  });

  $('btn-email-register').addEventListener('click', async () => {
    try {
      const email = $('login-email').value.trim();
      await api('/api/auth/register', {
        method: 'POST',
        body: { email, displayName: email.split('@')[0], password: $('login-password').value },
      });
      await refreshMe();
      await loadContests();
      toast('Account created!', 'success');
    } catch (e) { toast(e.message, 'error'); }
  });

  $('btn-join-contest').addEventListener('click', async () => {
    try {
      const res = await api('/api/contests/join', {
        method: 'POST',
        body: { accessCode: $('join-access-code').value.trim() },
      });
      toast(`Joined ${res.title}!`, 'success');
      await loadContests();
      await openContest(res.contestId);
    } catch (e) { toast(e.message, 'error'); }
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
      if (isTauri() && password) {
        await window.__TAURI__.core.invoke('save_oj_credentials', {
          platform: state.selectedOjTab,
          handle,
          password,
        });
      } else if (password) {
        localStorage.setItem(`vj_local_${state.selectedOjTab}`, JSON.stringify({ handle, hasLocalPass: true }));
      }
      await api('/api/oj/link', { method: 'POST', body: { platform: state.selectedOjTab, handle } });
      await refreshMe();
      toast(`Linked ${state.selectedOjTab} handle @${handle} (credentials kept local).`, 'success');
    } catch (err) { toast(err.message, 'error'); }
  });

  $('cp-platform').addEventListener('change', (e) => {
    const isLoc = e.target.value === 'local';
    $('cp-extid-wrap').classList.toggle('hidden', isLoc);
    $('cp-tests-wrap').classList.toggle('hidden', !isLoc);
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
          title: $('cp-title').value.trim(),
          statement: $('cp-statement').value.trim(),
          sampleTests: isLoc ? [{ input: $('cp-sin').value, output: $('cp-sout').value }] : [],
        },
      });
      toast('Problem saved!', 'success');
      $('btn-nav-coordinator').click();
    } catch (err) { toast(err.message, 'error'); }
  });

  $('create-contest-form').addEventListener('submit', async (e) => {
    e.preventDefault();
    try {
      const problemIds = Array.from($('cc-problem-list').querySelectorAll('input:checked')).map((x) => Number(x.value));
      const now = Math.floor(Date.now() / 1000);
      const res = await api('/api/contests', {
        method: 'POST',
        body: {
          title: $('cc-title').value.trim(),
          accessCode: $('cc-code').value.trim() || null,
          startTime: now,
          endTime: now + Number($('cc-duration').value || 120) * 60,
          penaltyMinutes: Number($('cc-penalty').value || 20),
          freezeMinutes: Number($('cc-freeze').value || 30),
          problemIds,
        },
      });
      navigator.clipboard?.writeText(res.access_code).catch(() => {});
      toast(`Contest created! Access Code: ${res.access_code}`, 'success');
      state.activeContestId = res.id;
      await loadContests();
    } catch (err) { toast(err.message, 'error'); }
  });

  $('btn-submit-webview').addEventListener('click', () => handleExternalSubmit().catch((e) => toast(e.message, 'error')));
  $('btn-submit-local-alt').addEventListener('click', () => handleLocalJudge(true).catch((e) => toast(e.message, 'error')));
  $('btn-submit-local').addEventListener('click', () => handleLocalJudge(false).catch((e) => toast(e.message, 'error')));
  $('btn-sync-verdicts').addEventListener('click', async () => {
    try {
      const r = await api(`/api/contests/${state.activeContestId}/sync`, { method: 'POST', body: {} });
      toast(`Synced ${r.syncedCount} new, ${r.updatedCount} updated.`, 'success');
      await Promise.all([loadSubmissions(state.activeContestId), loadStandings(state.activeContestId)]);
    } catch (e) { toast(e.message, 'error'); }
  });
  $('btn-sync-all').addEventListener('click', async () => {
    try {
      const r = await api(`/api/contests/${state.activeContestId}/sync-all`, { method: 'POST', body: {} });
      toast(`Synced ${r.totalSynced} new across ${r.checkedHandles} handles.`, 'success');
      await Promise.all([loadSubmissions(state.activeContestId), loadStandings(state.activeContestId)]);
    } catch (e) { toast(e.message, 'error'); }
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
    } catch (e) { toast(e.message, 'error'); }
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

(async function init() {
  wireEvents();
  await handleSupabaseOAuthReturn();
  await refreshMe();
  await loadContests();
  setInterval(updateTimer, 1000);
})();
