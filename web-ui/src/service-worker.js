// A generic service worker for a PWA
//
// CACHE_NAME is the cache version. Bump it whenever any file under web-ui/src
// changes: the new bytes make the browser install this worker, and `activate`
// deletes the stale cache. This is how installed PWAs get updates without a
// reinstall.
const CACHE_NAME = 'my-pwa-cache-v8';

// Everything the app shell needs, precached on install. `cache.addAll` is
// all-or-nothing — a single 404 aborts the install — so keep this in sync with
// the files under web-ui/src (see CLAUDE.md). Directory paths ('/search/')
// resolve to their index.html and are what the in-app nav links request.
const urlsToCache = [
  // App shell
  '/',
  '/index.html',
  '/output.css',
  '/manifest.json',
  '/theme-init.js',
  '/theme.js',
  '/register-sw.js',
  '/notifications.js',

  // Icons / images
  '/favicon.ico',
  '/icon512_maskable.png',
  '/icon512_rounded.png',

  // Shared components
  '/components/index.js',
  '/components/lib/html.js',
  '/components/hq-badge.js',
  '/components/hq-button.js',
  '/components/hq-card.js',
  '/components/hq-empty-state.js',
  '/components/hq-file-tree.js',
  '/components/hq-icon.js',
  '/components/hq-modal.js',
  '/components/hq-nav-rail.js',
  '/components/hq-page-shell.js',
  '/components/hq-pagination.js',
  '/components/hq-select.js',
  '/components/hq-spinner.js',
  '/components/hq-stat-card.js',
  '/components/hq-state-view.js',

  // Search
  '/search/',
  '/search/index.html',
  '/search/index.js',
  '/search/note-modal.js',
  '/search/search-result.js',

  // Chat
  '/chat/',
  '/chat/index.html',
  '/chat/index.js',
  '/chat/attachment-bubble.js',
  '/chat/message-bubble.js',
  '/chat/img/dog1.png',
  '/chat/img/dog2.png',
  '/chat/img/dog3.png',

  // Chat sessions
  '/chat/sessions/',
  '/chat/sessions/index.html',
  '/chat/sessions/index.js',
  '/chat/sessions/session-item.js',

  // Loops
  '/loops/',
  '/loops/index.html',
  '/loops/index.js',

  // Metrics
  '/metrics/',
  '/metrics/index.html',
  '/metrics/index.js',

  // Skills
  '/skills/',
  '/skills/index.html',
  '/skills/index.js',
  '/skills/code-editor.js',

  // Components gallery
  '/components-gallery/',
  '/components-gallery/index.html',
  '/components-gallery/index.js',

  // Vendor
  '/vendor/echarts.simple.min.js',
  '/vendor/highlight.min.js',
  '/vendor/htm.js',
  '/vendor/marked.min.js',
  '/vendor/codemirror/codemirror.min.css',
  '/vendor/codemirror/codemirror.min.js',
  '/vendor/codemirror/theme/dracula.min.css',
  '/vendor/codemirror/mode/javascript.min.js',
  '/vendor/codemirror/mode/markdown.min.js',
  '/vendor/codemirror/mode/python.min.js',
  '/vendor/codemirror/mode/shell.min.js',
  '/vendor/codemirror/mode/xml.min.js',
  '/vendor/codemirror/mode/yaml.min.js',
];

self.addEventListener('install', (event) => {
  event.waitUntil(
    caches.open(CACHE_NAME).then((cache) => {
      console.log('Opened cache');
      return cache.addAll(urlsToCache);
    }),
  );
});

self.addEventListener('fetch', (event) => {
  const { request } = event;

  // Only GET requests are cacheable. Let the browser handle mutations
  // (POST/PATCH/DELETE) directly so a network failure isn't turned into a
  // cache lookup, which resolves to undefined for these methods.
  if (request.method !== 'GET') return;

  const url = new URL(request.url);

  // Never intercept API requests. SSE streams (e.g. /api/chat/{id}/stream)
  // must reach the network directly; cloning and caching a long-lived
  // streaming body breaks it.
  if (url.pathname.startsWith('/api/')) return;

  // Only same-origin assets are ours to cache.
  if (url.origin !== self.location.origin) return;

  // HTML navigations: network-first so a freshly deployed app shell is picked
  // up on reload, falling back to the cache when offline.
  if (request.mode === 'navigate') {
    event.respondWith(
      fetch(request)
        .then((response) => {
          const responseClone = response.clone();
          caches.open(CACHE_NAME).then((cache) => {
            cache.put(request, responseClone);
          });
          return response;
        })
        .catch(async () => {
          // respondWith requires a Response, so return a network error when
          // there's no cached match.
          return (await caches.match(request)) || Response.error();
        }),
    );
    return;
  }

  // Static assets (JS/CSS/images): cache-first so repeat loads are instant.
  // The cache is versioned — a deploy that bumps CACHE_NAME refills it — and
  // the worker is only registered over HTTPS (see register-sw.js), so dev
  // reloads always hit the network instead of serving stale files.
  event.respondWith(
    caches.match(request).then((cached) => {
      if (cached) return cached;
      return fetch(request).then((response) => {
        const responseClone = response.clone();
        caches.open(CACHE_NAME).then((cache) => {
          cache.put(request, responseClone);
        });
        return response;
      });
    }),
  );
});

self.addEventListener('activate', (event) => {
  const cacheAllowlist = [CACHE_NAME];

  event.waitUntil(
    caches.keys().then((cacheNames) => {
      return Promise.all(
        cacheNames.map((cacheName) => {
          if (cacheAllowlist.indexOf(cacheName) === -1) {
            return caches.delete(cacheName);
          } else {
            return null;
          }
        }),
      );
    }),
  );
});

// Listen for push events and show notifications
self.addEventListener('push', (event) => {
  if (!event.data) {
    console.warn('Empty push payload');
    return;
  }

  event.waitUntil(handlePushNotification(event));
});

async function handlePushNotification(event) {
  // A payload can be text or a json object

  let title;
  let options;

  try {
    payload = await event.data.json();
    title = payload.title;
    options = {
      body: payload.body,
      data: payload.data,
    };
    isJson = true;
  } catch (_) {
    try {
      const text = await event.data.text();
      const _title = text;
      options = {
        body: text,
        icon: 'icon.png',
        badge: 'badge.png',
      };
      return;
    } catch (e) {
      console.error('Failed to read push data as text', e);
      return;
    }
  }
  await self.registration.showNotification(title, options);
}

// Handle the user clicking a push notification on desktop and linking
// to the URL specified by the notification payload
self.addEventListener('notificationclick', (event) => {
  // Keep the SW alive while we do async work.
  event.waitUntil(handleClick(event));
});

async function handleClick(event) {
  const url = event.notification.data?.url || '/';
  event.notification.close(); // hide UI

  // 1️⃣ Try to focus an existing tab that already has the URL
  const allClients = await self.clients.matchAll({
    type: 'window',
    includeUncontrolled: true, // also see pages not yet controlled by SW
  });

  for (const client of allClients) {
    const u = new URL(client.url);
    if (u.pathname + u.search + u.hash === url) {
      return client.focus(); // bring it to front
    }
  }

  // 2️⃣ No matching tab → open a brand‑new one.
  const absoluteUrl = new URL(url, self.registration.scope).href;
  return clients.openWindow(absoluteUrl);
}
