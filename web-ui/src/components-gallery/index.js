// Gallery page: renders a section per component.
// Each entry in `sections` is { title, render(container) }.

import { toggleDark } from '/theme.js';

const sections = [];

function darkToggle() {
  toggleDark();
}

function pageShellDemo(c) {
  c.innerHTML = `
    <h2 class="text-xl font-semibold mb-1 text-gray-900 dark:text-gray-100">&lt;hq-page-shell&gt;</h2>
    <p class="text-sm text-gray-600 dark:text-gray-400 mb-4">
      Wraps gradient background + safe-area padding + "Back to" link. Used by every page.
      <code class="text-xs bg-gray-100 dark:bg-gray-700 px-1 rounded">attrs: title, back-href, back-label</code>
    </p>
    <div class="border rounded-lg p-4 bg-gray-50 dark:bg-gray-900">
      <p class="text-sm text-gray-600 dark:text-gray-400 mb-2">Live demo (this page uses hq-page-shell):</p>
      <hq-page-shell meta-title="Gallery Demo" back-href="/" back-label="Home">
        <p class="text-sm">This content is slotted inside &lt;hq-page-shell&gt;'s <code>&lt;main&gt;</code>.</p>
      </hq-page-shell>
    </div>
  `;
}
sections.push({ title: 'hq-page-shell', render: pageShellDemo });

function cardDemo(c) {
  c.innerHTML = `
    <h2 class="text-xl font-semibold mb-1 text-gray-900 dark:text-gray-100">&lt;hq-card&gt;</h2>
    <p class="text-sm text-gray-600 dark:text-gray-400 mb-4">
      Surface container with optional header. <code class="text-xs bg-gray-100 dark:bg-gray-700 px-1 rounded">attrs: max-width (2xl|4xl|6xl|full)</code>
    </p>
    <div class="grid gap-4">
      <hq-card max-width="full">
        <div slot="header">
          <h3 class="text-lg font-semibold text-gray-900 dark:text-white">Card with header</h3>
          <p class="text-sm text-gray-600 dark:text-gray-400">This is the header slot.</p>
        </div>
        <p class="text-sm text-gray-700 dark:text-gray-300">This is the card body. The header has a bottom border separator.</p>
      </hq-card>
      <hq-card max-width="full">
        <p class="text-sm text-gray-700 dark:text-gray-300">Card without a header — just body content.</p>
      </hq-card>
    </div>
  `;
}
sections.push({ title: 'hq-card', render: cardDemo });

function buttonDemo(c) {
  c.innerHTML = `
    <h2 class="text-xl font-semibold mb-1 text-gray-900 dark:text-gray-100">&lt;hq-button&gt;</h2>
    <p class="text-sm text-gray-600 dark:text-gray-400 mb-4">
      Styled button with variants. <code class="text-xs bg-gray-100 dark:bg-gray-700 px-1 rounded">attrs: variant (primary|secondary|ghost|danger), disabled</code>
    </p>
    <div class="flex flex-wrap gap-3">
      <hq-button>Primary</hq-button>
      <hq-button variant="secondary">Secondary</hq-button>
      <hq-button variant="ghost">Ghost</hq-button>
      <hq-button variant="danger">Danger</hq-button>
      <hq-button disabled>Disabled</hq-button>
    </div>
  `;
}
sections.push({ title: 'hq-button', render: buttonDemo });

function badgeDemo(c) {
  c.innerHTML = `
    <h2 class="text-xl font-semibold mb-1 text-gray-900 dark:text-gray-100">&lt;hq-badge&gt;</h2>
    <p class="text-sm text-gray-600 dark:text-gray-400 mb-4">
      Colored pill badge. <code class="text-xs bg-gray-100 dark:bg-gray-700 px-1 rounded">attrs: tone (blue|green|yellow|red|gray)</code>
    </p>
    <div class="flex flex-wrap gap-2">
      <hq-badge tone="blue">blue</hq-badge>
      <hq-badge tone="green">green</hq-badge>
      <hq-badge tone="yellow">yellow</hq-badge>
      <hq-badge tone="red">red</hq-badge>
      <hq-badge>gray (default)</hq-badge>
    </div>
  `;
}
sections.push({ title: 'hq-badge', render: badgeDemo });

function spinnerDemo(c) {
  c.innerHTML = `
    <h2 class="text-xl font-semibold mb-1 text-gray-900 dark:text-gray-100">&lt;hq-spinner&gt;</h2>
    <p class="text-sm text-gray-600 dark:text-gray-400 mb-4">
      Loading spinner. <code class="text-xs bg-gray-100 dark:bg-gray-700 px-1 rounded">attrs: size (sm|md|lg), tone</code>
    </p>
    <div class="flex items-end gap-6">
      <div class="text-center">
        <hq-spinner size="sm"></hq-spinner>
        <p class="mt-2 text-xs text-gray-500">sm</p>
      </div>
      <div class="text-center">
        <hq-spinner size="md"></hq-spinner>
        <p class="mt-2 text-xs text-gray-500">md</p>
      </div>
      <div class="text-center">
        <hq-spinner size="lg"></hq-spinner>
        <p class="mt-2 text-xs text-gray-500">lg</p>
      </div>
    </div>
  `;
}
sections.push({ title: 'hq-spinner', render: spinnerDemo });

function iconDemo(c) {
  c.innerHTML = `
    <h2 class="text-xl font-semibold mb-1 text-gray-900 dark:text-gray-100">&lt;hq-icon&gt;</h2>
    <p class="text-sm text-gray-600 dark:text-gray-400 mb-4">
      Inline SVG icon library. <code class="text-xs bg-gray-100 dark:bg-gray-700 px-1 rounded">attrs: name, size (sm|md|lg)</code>
    </p>
    <div class="grid grid-cols-2 sm:grid-cols-4 gap-4 text-gray-700 dark:text-gray-300">
      <div class="flex items-center gap-2"><hq-icon name="chevron-left"></hq-icon> <span class="text-sm">chevron-left</span></div>
      <div class="flex items-center gap-2"><hq-icon name="chevron-right"></hq-icon> <span class="text-sm">chevron-right</span></div>
      <div class="flex items-center gap-2"><hq-icon name="search"></hq-icon> <span class="text-sm">search</span></div>
      <div class="flex items-center gap-2"><hq-icon name="chat"></hq-icon> <span class="text-sm">chat</span></div>
      <div class="flex items-center gap-2"><hq-icon name="sessions"></hq-icon> <span class="text-sm">sessions</span></div>
      <div class="flex items-center gap-2"><hq-icon name="skills"></hq-icon> <span class="text-sm">skills</span></div>
      <div class="flex items-center gap-2"><hq-icon name="loops"></hq-icon> <span class="text-sm">loops</span></div>
      <div class="flex items-center gap-2"><hq-icon name="metrics"></hq-icon> <span class="text-sm">metrics</span></div>
      <div class="flex items-center gap-2"><hq-icon name="close"></hq-icon> <span class="text-sm">close</span></div>
      <div class="flex items-center gap-2"><hq-icon name="alert"></hq-icon> <span class="text-sm">alert</span></div>
      <div class="flex items-center gap-2"><hq-icon name="plus"></hq-icon> <span class="text-sm">plus</span></div>
    </div>
  `;
}
sections.push({ title: 'hq-icon', render: iconDemo });

function stateViewDemo(c) {
  c.innerHTML = `
    <h2 class="text-xl font-semibold mb-1 text-gray-900 dark:text-gray-100">&lt;hq-state-view&gt;</h2>
    <p class="text-sm text-gray-600 dark:text-gray-400 mb-4">
      State-driven slot switcher. <code class="text-xs bg-gray-100 dark:bg-gray-700 px-1 rounded">attrs: state (loading|error|empty|content)</code>
    </p>
    <div class="space-y-4">
      <div class="max-w-xs">
        <hq-select id="state-view-select" label="State">
          <option value="loading">loading</option>
          <option value="error">error</option>
          <option value="empty">empty</option>
          <option value="content" selected>content</option>
        </hq-select>
      </div>
      <hq-state-view id="state-demo" state="content">
        <p>This is the content slot.</p>
        <div slot="error" class="text-center py-8">
          <div class="flex justify-center"><hq-icon name="alert" size="lg" tone="text-red-400"></hq-icon></div>
          <p class="mt-2 text-red-600 dark:text-red-400">Something went wrong.</p>
          <hq-button variant="secondary" class="mt-4">Retry</hq-button>
        </div>
        <hq-empty-state slot="empty" icon="search" title="No results found"></hq-empty-state>
      </hq-state-view>
    </div>
  `;
  const sel = c.querySelector('#state-view-select');
  const sv = c.querySelector('#state-demo');
  sel.addEventListener('change', (e) =>
    sv.setAttribute('state', e.detail.value),
  );
}
sections.push({ title: 'hq-state-view', render: stateViewDemo });

function selectDemo(c) {
  c.innerHTML = `
    <h2 class="text-xl font-semibold mb-1 text-gray-900 dark:text-gray-100"><hq-select></h2>
    <p class="text-sm text-gray-600 dark:text-gray-400 mb-4">
      Styled dropdown. <code class="text-xs bg-gray-100 dark:bg-gray-700 px-1 rounded">attrs: label, value, disabled</code>
      <span class="ml-2">Emits <code>change</code> with <code>detail.value</code>.</span>
    </p>
    <div class="max-w-xs space-y-4">
      <hq-select id="select-demo" label="Pick a fruit" value="apple">
        <option value="apple">Apple</option>
        <option value="banana">Banana</option>
        <option value="cherry">Cherry</option>
      </hq-select>
      <hq-select label="Disabled" disabled>
        <option value="1">Option one</option>
        <option value="2">Option two</option>
      </hq-select>
      <p class="text-sm text-gray-600 dark:text-gray-400">Current: <span id="select-current" class="font-semibold">apple</span></p>
    </div>
  `;
  const sel = c.querySelector('#select-demo');
  const current = c.querySelector('#select-current');
  sel.addEventListener('change', (e) => {
    current.textContent = e.detail.value;
  });
}
sections.push({ title: 'hq-select', render: selectDemo });

function emptyStateDemo(c) {
  c.innerHTML = `
    <h2 class="text-xl font-semibold mb-1 text-gray-900 dark:text-gray-100">&lt;hq-empty-state&gt;</h2>
    <p class="text-sm text-gray-600 dark:text-gray-400 mb-4">
      Centered empty/placeholder message. <code class="text-xs bg-gray-100 dark:bg-gray-700 px-1 rounded">attrs: icon, title</code>
    </p>
    <hq-empty-state icon="search" title="No skills found">
      Try adjusting your search or browse all available skills.
    </hq-empty-state>
  `;
}
sections.push({ title: 'hq-empty-state', render: emptyStateDemo });

function modalDemo(c) {
  c.innerHTML = `
    <h2 class="text-xl font-semibold mb-1 text-gray-900 dark:text-gray-100">&lt;hq-modal&gt;</h2>
    <p class="text-sm text-gray-600 dark:text-gray-400 mb-4">
      Full-screen overlay modal. <code class="text-xs bg-gray-100 dark:bg-gray-700 px-1 rounded">attrs: open (boolean)</code>
    </p>
    <hq-button id="modal-open-btn">Open modal</hq-button>
    <hq-modal id="modal-demo">
      <h3 class="text-lg font-semibold mb-2 text-gray-900 dark:text-white">Modal title</h3>
      <p class="text-sm text-gray-700 dark:text-gray-300 mb-4">This is a modal dialog. Click the backdrop or press Escape to close.</p>
      <hq-button id="modal-close-btn" variant="secondary">Close</hq-button>
    </hq-modal>
  `;
  const modal = c.querySelector('#modal-demo');
  c.querySelector('#modal-open-btn').addEventListener('click', () =>
    modal.setAttribute('open', ''),
  );
  c.querySelector('#modal-close-btn').addEventListener('click', () =>
    modal.removeAttribute('open'),
  );
}
sections.push({ title: 'hq-modal', render: modalDemo });

function paginationDemo(c) {
  c.innerHTML = `
    <h2 class="text-xl font-semibold mb-1 text-gray-900 dark:text-gray-100">&lt;hq-pagination&gt;</h2>
    <p class="text-sm text-gray-600 dark:text-gray-400 mb-4">
      Page navigation with 5-page window. <code class="text-xs bg-gray-100 dark:bg-gray-700 px-1 rounded">attrs: page, total-pages</code>
      <span class="ml-2">Emits <code>page-change</code> event.</span>
    </p>
    <hq-pagination id="pag-demo" page="3" total-pages="10"></hq-pagination>
    <p class="mt-4 text-sm">Current page: <span id="pag-current" class="font-semibold">3</span></p>
  `;
  const pag = c.querySelector('#pag-demo');
  const current = c.querySelector('#pag-current');
  pag.addEventListener('page-change', (e) => {
    current.textContent = e.detail.page;
  });
}
sections.push({ title: 'hq-pagination', render: paginationDemo });

function fileTreeDemo(c) {
  const files = JSON.stringify([
    { path: 'SKILL.md', is_directory: false },
    { path: 'src/', is_directory: true },
    { path: 'src/index.js', is_directory: false },
    { path: 'src/helpers.js', is_directory: false },
    { path: 'README.md', is_directory: false },
  ]);
  c.innerHTML = `
    <h2 class="text-xl font-semibold mb-1 text-gray-900 dark:text-gray-100">&lt;hq-file-tree&gt;</h2>
    <p class="text-sm text-gray-600 dark:text-gray-400 mb-4">
      File list with directory expansion. <code class="text-xs bg-gray-100 dark:bg-gray-700 px-1 rounded">attrs: files (JSON), selected</code>
      <span class="ml-2">Emits <code>file-select</code> event.</span>
    </p>
    <div class="grid sm:grid-cols-2 gap-4">
      <div>
        <p class="text-sm font-medium mb-2 text-gray-700 dark:text-gray-300">Tree:</p>
        <hq-file-tree id="tree-demo" files='${files}' selected="SKILL.md"></hq-file-tree>
      </div>
      <div>
        <p class="text-sm font-medium mb-2 text-gray-700 dark:text-gray-300">Selected:</p>
        <p id="tree-selected" class="text-sm font-mono bg-gray-100 dark:bg-gray-700 px-2 py-1 rounded">SKILL.md</p>
      </div>
    </div>
  `;
  const tree = c.querySelector('#tree-demo');
  const sel = c.querySelector('#tree-selected');
  tree.addEventListener('file-select', (e) => {
    sel.textContent = e.detail.path;
  });
}
sections.push({ title: 'hq-file-tree', render: fileTreeDemo });

function statCardDemo(c) {
  c.innerHTML = `
    <h2 class="text-xl font-semibold mb-1 text-gray-900 dark:text-gray-100">&lt;hq-stat-card&gt;</h2>
    <p class="text-sm text-gray-600 dark:text-gray-400 mb-4">
      KPI stat card with label and value. <code class="text-xs bg-gray-100 dark:bg-gray-700 px-1 rounded">attrs: label, value</code>
    </p>
    <div class="grid grid-cols-1 sm:grid-cols-3 gap-4">
      <hq-stat-card label="Total Tokens" value="1.2M"></hq-stat-card>
      <hq-stat-card label="Avg / Day" value="48K"></hq-stat-card>
      <hq-stat-card label="Est. Cost">
        <span class="text-green-600 dark:text-green-400">$24.50</span>
      </hq-stat-card>
    </div>
  `;
}
sections.push({ title: 'hq-stat-card', render: statCardDemo });

// Render all sections
function renderAll() {
  const root = document.getElementById('gallery-root');
  if (!root) return;
  root.innerHTML = `
    <div class="max-w-6xl mx-auto px-4 sm:px-6 lg:px-8 py-8">
      <div class="flex items-center justify-between mb-8 flex-wrap gap-4">
        <div>
          <h1 class="text-3xl font-bold text-gray-900 dark:text-white">Components Gallery</h1>
          <p class="mt-2 text-gray-600 dark:text-gray-400">Visual reference for all hq web components.</p>
        </div>
        <button id="hq-dark-toggle" class="px-4 py-2 bg-gray-800 dark:bg-gray-200 text-white dark:text-gray-900 rounded-md hover:opacity-80 transition-opacity">
          Toggle dark mode
        </button>
      </div>
      <p class="text-sm text-gray-500 dark:text-gray-400 mb-12">
        Components appear here as they are built. See
        <a href="/components/README.md" class="text-blue-600 dark:text-blue-400 underline">the README</a>
        for the full list.
      </p>
    </div>
  `;
  const sectionsContainer = document.createElement('div');
  sectionsContainer.className =
    'max-w-6xl mx-auto px-4 sm:px-6 lg:px-8 pb-12 space-y-12';
  for (const section of sections) {
    const wrap = document.createElement('section');
    wrap.className = 'bg-white dark:bg-gray-800 rounded-xl shadow-sm p-6';
    section.render(wrap);
    sectionsContainer.appendChild(wrap);
  }
  root.appendChild(sectionsContainer);

  const toggle = document.getElementById('hq-dark-toggle');
  if (toggle) toggle.addEventListener('click', darkToggle);
}

if (document.readyState === 'loading') {
  document.addEventListener('DOMContentLoaded', renderAll);
} else {
  renderAll();
}
