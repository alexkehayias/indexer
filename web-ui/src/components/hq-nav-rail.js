/**
 * <hq-nav-rail> — desktop (md+) 80px icon navigation rail.
 *
 * @attr {string} active - destination key to highlight: search|chat|sessions|
 *   skills|loops|metrics. The active item is shown in its fixed accent tint.
 *
 * Each destination keeps one accent everywhere it appears: Search = teal,
 * Chat = amber, Sessions = rust, Skills = moss, Loops = cobalt, Metrics = mauve.
 *
 * Hidden below md; mobile pages navigate from the home tile grid. The host is
 * display:contents so the inner <aside> is the flex item.
 */
import { html } from '/components/lib/html.js';
import '/components/hq-icon.js';

const ITEMS = [
  { key: 'search', href: '/search/', label: 'Search', icon: 'search' },
  { key: 'chat', href: '/chat/', label: 'Chat', icon: 'chat' },
  {
    key: 'sessions',
    href: '/chat/sessions/',
    label: 'Sessions',
    icon: 'sessions',
  },
  { key: 'skills', href: '/skills/', label: 'Skills', icon: 'skills' },
  { key: 'loops', href: '/loops/', label: 'Loops', icon: 'loops' },
  { key: 'metrics', href: '/metrics/', label: 'Metrics', icon: 'metrics' },
];

const ACCENT = {
  search: 'bg-teal-tint text-teal',
  chat: 'bg-amber-tint text-amber',
  sessions: 'bg-rust-tint text-rust',
  skills: 'bg-moss-tint text-moss',
  loops: 'bg-cobalt-tint text-cobalt',
  metrics: 'bg-mauve-tint text-mauve',
};

class HqNavRail extends HTMLElement {
  static observedAttributes = ['active'];

  #update() {
    const active = this.getAttribute('active');
    const result = html`<aside class="hidden md:flex w-20 shrink-0 flex-col items-center gap-1.5 border-r border-line bg-card py-4">
      <a href="/" title="Home" class="flex size-11 items-center justify-center rounded-icon text-muted transition-colors hover:bg-mist hover:text-ink">
        <hq-icon name="home" size="lg"></hq-icon>
      </a>
      <div class="my-1 h-px w-8 bg-line"></div>
      ${ITEMS.map(
        (item) => html`<a
          href="${item.href}"
          title="${item.label}"
          class="flex size-11 items-center justify-center rounded-icon transition-colors ${active === item.key ? ACCENT[item.key] : 'text-muted hover:bg-mist hover:text-ink'}"
        >
          <hq-icon name="${item.icon}" size="lg"></hq-icon>
        </a>`,
      )}
    </aside>`;
    this.innerHTML = result.value;
  }

  connectedCallback() {
    this.style.display = 'contents';
    this.#update();
  }

  attributeChangedCallback() {
    this.#update();
  }
}

customElements.define('hq-nav-rail', HqNavRail);
