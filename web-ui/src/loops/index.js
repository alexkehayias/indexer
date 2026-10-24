// Loops page: a tabbed list of loops and channels, plus a detail view for each.
import { html } from '/components/lib/html.js';

/* ===== State ===== */
let loops = [];
let channels = [];

/* ===== Elements ===== */
const pageShell = document.getElementById('page-shell');
const listView = document.getElementById('list-view');
const loopDetailView = document.getElementById('loop-detail-view');
const channelDetailView = document.getElementById('channel-detail-view');

const tabs = document.getElementById('tabs');
const loopsPanel = document.getElementById('loops-panel');
const channelsPanel = document.getElementById('channels-panel');
const loopsState = document.getElementById('loops-state');
const channelsState = document.getElementById('channels-state');
const loopsErrorMsg = document.getElementById('loops-error-msg');
const channelsErrorMsg = document.getElementById('channels-error-msg');
const loopGrid = document.getElementById('loop-grid');
const channelGrid = document.getElementById('channel-grid');

/* ===== Data ===== */
async function loadData() {
  const [loopsRes, channelsRes] = await Promise.all([
    fetch('/api/loops'),
    fetch('/api/channels'),
  ]);
  if (!loopsRes.ok) throw new Error(`HTTP ${loopsRes.status}`);
  if (!channelsRes.ok) throw new Error(`HTTP ${channelsRes.status}`);
  loops = (await loopsRes.json()).loops || [];
  channels = (await channelsRes.json()).channels || [];
}

function subscribersOf(id) {
  return loops.filter((loop) => loop.channels.includes(id));
}

/* ===== List View ===== */
function showList(activeTab) {
  listView.classList.remove('hidden');
  loopDetailView.classList.add('hidden');
  channelDetailView.classList.add('hidden');
  pageShell.setAttribute('meta-title', 'Loops');
  pageShell.setAttribute('back-href', '/');
  pageShell.setAttribute('back-label', 'Home');
  selectTab(activeTab);
  loadList();
}

const TAB_BASE =
  'rounded-[9px] px-4 py-1.5 text-sm font-medium transition-colors';
const TAB_ACTIVE = `${TAB_BASE} bg-card text-ink shadow-sm`;
const TAB_IDLE = `${TAB_BASE} text-muted hover:text-ink`;

function selectTab(tab) {
  const isLoops = tab !== 'channels';
  loopsPanel.classList.toggle('hidden', !isLoops);
  channelsPanel.classList.toggle('hidden', isLoops);

  const loopsTab = document.getElementById('tab-loops');
  const channelsTab = document.getElementById('tab-channels');
  loopsTab.className = isLoops ? TAB_ACTIVE : TAB_IDLE;
  channelsTab.className = isLoops ? TAB_IDLE : TAB_ACTIVE;
  loopsTab.setAttribute('aria-selected', String(isLoops));
  channelsTab.setAttribute('aria-selected', String(!isLoops));
}

tabs.addEventListener('click', (event) => {
  const button = event.target.closest('button[data-tab]');
  if (!button) return;
  selectTab(button.dataset.tab);

  const url = new URL(window.location.href);
  if (button.dataset.tab === 'channels')
    url.searchParams.set('tab', 'channels');
  else url.searchParams.delete('tab');
  window.history.replaceState({}, '', url);
});

async function loadList() {
  loopsState.setAttribute('state', 'loading');
  channelsState.setAttribute('state', 'loading');
  try {
    await loadData();
    renderLoops();
    renderChannels();
  } catch (err) {
    loopsErrorMsg.textContent = `Failed to load loops: ${err.message}`;
    channelsErrorMsg.textContent = `Failed to load channels: ${err.message}`;
    loopsState.setAttribute('state', 'error');
    channelsState.setAttribute('state', 'error');
  }
}

document.getElementById('loops-retry').addEventListener('click', loadList);
document.getElementById('channels-retry').addEventListener('click', loadList);

function renderLoops() {
  if (loops.length === 0) {
    loopsState.setAttribute('state', 'empty');
    return;
  }
  loopGrid.innerHTML = loops.map(loopCard).map(toValue).join('');
  loopsState.setAttribute('state', 'content');
}

function renderChannels() {
  if (channels.length === 0) {
    channelsState.setAttribute('state', 'empty');
    return;
  }
  channelGrid.innerHTML = channels.map(channelCard).map(toValue).join('');
  channelsState.setAttribute('state', 'content');
}

/* ===== Cards ===== */
function loopCard(loop) {
  return html`
    <a href="/loops/?loop=${loop.id}"
       class="flex flex-col rounded-card border border-line bg-card p-4 transition-shadow hover:shadow-lg/5 md:p-[22px]">
      <div class="flex items-start justify-between gap-3">
        <div class="min-w-0">
          <h3 class="font-semibold text-ink">Loop</h3>
          <p class="mt-0.5 truncate font-mono text-xs text-muted">${loop.id}</p>
        </div>
        ${loop.running ? html`<hq-badge tone="moss">running</hq-badge>` : html`<hq-badge tone="gray">stopped</hq-badge>`}
      </div>
      <div class="mt-3 flex flex-wrap gap-1.5">
        ${
          loop.channels.length
            ? loop.channels.map(
                (channel) => html`<hq-badge tone="teal">${channel}</hq-badge>`,
              )
            : html`<span class="text-xs text-muted">No channels</span>`
        }
      </div>
    </a>
  `;
}

function channelCard(channel) {
  const count = subscribersOf(channel.id).length;
  return html`
    <a href="/loops/?channel=${channel.id}"
       class="flex flex-col rounded-card border border-line bg-card p-4 transition-shadow hover:shadow-lg/5 md:p-[22px]">
      <div class="flex items-start justify-between gap-3">
        <h3 class="min-w-0 truncate font-mono font-semibold text-ink">${channel.id}</h3>
        <div class="flex shrink-0 items-center gap-1.5">
          ${channel.created_by ? html`<hq-badge tone="amber">agent</hq-badge>` : ''}
          <hq-badge tone="${sourceTone(channel.source)}">${channel.source}</hq-badge>
        </div>
      </div>
      <p class="mt-3 text-sm text-muted">${count} loop${count === 1 ? '' : 's'} subscribed</p>
    </a>
  `;
}

/* ===== Loop Detail ===== */
async function showLoopDetail(id) {
  listView.classList.add('hidden');
  channelDetailView.classList.add('hidden');
  loopDetailView.classList.remove('hidden');
  pageShell.setAttribute('meta-title', 'Loop');
  pageShell.setAttribute('back-href', '/loops/');
  pageShell.setAttribute('back-label', 'Loops');

  document.getElementById('loop-detail-id').textContent = id;
  document.getElementById('loop-detail-status').innerHTML = '';
  const body = document.getElementById('loop-detail-body');
  body.innerHTML = spinner();

  try {
    await loadData();
  } catch (err) {
    body.innerHTML = errorState('Failed to load loop', err.message);
    return;
  }

  const loop = loops.find((l) => l.id === id);
  if (!loop) {
    body.innerHTML = errorState(
      'Loop not found',
      'No loop with this id exists.',
    );
    return;
  }

  document
    .getElementById('loop-detail-status')
    .appendChild(
      badge(
        loop.running ? 'running' : 'stopped',
        loop.running ? 'moss' : 'gray',
      ),
    );

  body.innerHTML = html`<div class="flex flex-col gap-6">
    ${section(
      'Subscribed channels',
      loop.channels.length
        ? html`<div class="flex flex-wrap gap-2">
            ${loop.channels.map((channel) => html`<a href="/loops/?channel=${channel}"><hq-badge tone="teal">${channel}</hq-badge></a>`)}
          </div>`
        : html`<p class="text-sm text-muted">This loop is not subscribed to any channels.</p>`,
    )}
    ${section(
      'Tools',
      loop.tools.length
        ? html`<div class="flex flex-wrap gap-2">${loop.tools.map((tool) => html`<hq-badge tone="gray">${tool}</hq-badge>`)}</div>`
        : html`<p class="text-sm text-muted">Using the default tools (bash, notify).</p>`,
    )}
    ${section(
      'System prompt',
      html`<pre class="overflow-auto rounded-[14px] bg-code p-4 font-mono text-[12.5px] leading-relaxed text-teal-tint">${loop.system_prompt}</pre>`,
    )}
    ${section('Created', html`<p class="text-sm text-body">${loop.created_at}</p>`)}
  </div>`.value;
}

/* ===== Channel Detail ===== */
async function showChannelDetail(id) {
  listView.classList.add('hidden');
  loopDetailView.classList.add('hidden');
  channelDetailView.classList.remove('hidden');
  pageShell.setAttribute('meta-title', id);
  pageShell.setAttribute('back-href', '/loops/?tab=channels');
  pageShell.setAttribute('back-label', 'Channels');

  document.getElementById('channel-detail-id').textContent = id;
  document.getElementById('channel-detail-badges').innerHTML = '';
  const body = document.getElementById('channel-detail-body');
  body.innerHTML = spinner();

  try {
    await loadData();
  } catch (err) {
    body.innerHTML = errorState('Failed to load channel', err.message);
    return;
  }

  const channel = channels.find((c) => c.id === id);
  if (!channel) {
    body.innerHTML = errorState(
      'Channel not found',
      'No channel with this id exists.',
    );
    return;
  }

  const badges = document.getElementById('channel-detail-badges');
  if (channel.created_by) badges.appendChild(badge('agent', 'amber'));
  badges.appendChild(badge(channel.source, sourceTone(channel.source)));

  const subscribers = subscribersOf(channel.id);
  body.innerHTML = html`<div class="flex flex-col gap-6">
    ${section(
      'Details',
      html`<dl class="grid grid-cols-[7rem_1fr] gap-y-2 text-sm">
        <dt class="text-muted">Source</dt><dd class="text-body">${channel.source}</dd>
        ${
          channel.created_by
            ? html`<dt class="text-muted">Owner</dt><dd class="font-mono text-body">${channel.created_by}</dd>`
            : ''
        }
        <dt class="text-muted">Debounce</dt><dd class="text-body">${channel.debounce_ms} ms</dd>
        <dt class="text-muted">Created</dt><dd class="text-body">${channel.created_at}</dd>
      </dl>`,
    )}
    ${section(
      `Subscribed loops (${subscribers.length})`,
      subscribers.length
        ? html`<div class="grid grid-cols-1 gap-4 sm:grid-cols-2">${subscribers.map(loopCard)}</div>`
        : html`<p class="text-sm text-muted">No loops subscribe to this channel.</p>`,
    )}
  </div>`.value;
}

/* ===== Helpers ===== */
function section(title, content) {
  return html`
    <section>
      <h2 class="mb-3 text-sm font-semibold uppercase tracking-wider text-body">${title}</h2>
      ${content}
    </section>
  `;
}

function sourceTone(source) {
  if (source === 'server') return 'mauve';
  if (source === 'webhook') return 'rust';
  return 'amber';
}

function badge(text, tone) {
  const el = document.createElement('hq-badge');
  el.setAttribute('tone', tone);
  el.textContent = text;
  return el;
}

function spinner() {
  return '<div class="flex justify-center py-8"><hq-spinner></hq-spinner></div>';
}

function errorState(title, message) {
  return html`<hq-empty-state icon="alert" title="${title}">
    <span>${message}</span>
  </hq-empty-state>`.value;
}

function toValue(result) {
  return result.value;
}

/* ===== Init ===== */
const params = new URLSearchParams(window.location.search);
const loopId = params.get('loop');
const channelId = params.get('channel');

if (loopId) {
  showLoopDetail(loopId);
} else if (channelId) {
  showChannelDetail(channelId);
} else {
  showList(params.get('tab') === 'channels' ? 'channels' : 'loops');
}
