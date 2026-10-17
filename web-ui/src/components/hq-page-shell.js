/**
 * <hq-page-shell> — page wrapper with the canvas background and "Back to" link.
 *
 * @attr {string} meta-title - sets document.title / browser tab (e.g. "Skills - HQ")
 * @attr {string} back-href - URL for the back link (default "/")
 * @attr {string} back-label - destination label, rendered as "Back to {label}" (default "Home")
 * @attr {string} nav-rail - optional destination key (search|chat|sessions|skills|
 *   loops|metrics). When set, an <hq-nav-rail> is shown on md+ and the back link
 *   is hidden there (the rail handles desktop navigation).
 * @slot default - page body, placed inside <main>
 *
 * Light DOM: original children are moved into a <main> container on first connect.
 */
class HqPageShell extends HTMLElement {
  static observedAttributes = [
    'meta-title',
    'back-href',
    'back-label',
    'nav-rail',
  ];

  #initialized = false;

  connectedCallback() {
    if (this.#initialized) return;
    this.#initialized = true;

    const fragment = document.createDocumentFragment();
    while (this.firstChild) fragment.appendChild(this.firstChild);

    this.innerHTML = `
      <div class="min-h-dvh bg-canvas pt-[env(safe-area-inset-top)] pr-[env(safe-area-inset-right)] pb-[env(safe-area-inset-bottom)] pl-[env(safe-area-inset-left)]">
        <div class="flex min-h-dvh">
          <hq-nav-rail id="hq-rail"></hq-nav-rail>
          <div class="flex-1 min-w-0">
            <div class="mx-auto flex w-full max-w-6xl flex-col items-start px-6 py-6 md:py-10">
              <a id="hq-back-link" href="/" class="inline-flex items-center text-teal-strong hover:text-teal transition-colors mb-6">
                <svg class="h-5 w-5 mr-2" fill="none" viewBox="0 0 24 24" stroke-width="2" stroke="currentColor">
                  <path stroke-linecap="round" stroke-linejoin="round" d="M15.75 19.5L8.25 12l7.5-7.5" />
                </svg>
                <span id="hq-back-label">Back to Home</span>
              </a>
              <main class="w-full text-ink"></main>
            </div>
          </div>
        </div>
      </div>
    `;

    this.querySelector('main').appendChild(fragment);
    this.#updateAttrs();
  }

  attributeChangedCallback() {
    if (this.#initialized) this.#updateAttrs();
  }

  #updateAttrs() {
    const title = this.getAttribute('meta-title');
    if (title) document.title = `${title} - HQ`;

    const navRail = this.getAttribute('nav-rail');
    const rail = this.querySelector('#hq-rail');
    const backLink = this.querySelector('#hq-back-link');
    if (rail) {
      if (navRail) {
        rail.setAttribute('active', navRail);
      } else {
        rail.remove();
      }
    }
    // The desktop rail replaces the back link; keep it for mobile navigation.
    if (backLink) {
      backLink.classList.toggle('md:hidden', Boolean(navRail));
    }

    const link = this.querySelector('#hq-back-link');
    if (link) {
      link.setAttribute('href', this.getAttribute('back-href') || '/');
      const label = this.querySelector('#hq-back-label');
      if (label) {
        const backLabel = this.getAttribute('back-label') || 'Home';
        label.textContent = `Back to ${backLabel}`;
      }
    }
  }
}

customElements.define('hq-page-shell', HqPageShell);
