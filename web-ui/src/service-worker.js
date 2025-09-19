// A generic service worker for a PWA
const CACHE_NAME = 'my-pwa-cache-v1';
const urlsToCache = [
  '/',
  '/index.html',
  '/output.css',
  '/index.js'
];

self.addEventListener('install', (event) => {
  event.waitUntil(
    caches.open(CACHE_NAME)
          .then((cache) => {
            console.log('Opened cache');
            return cache.addAll(urlsToCache);
          })
  );
});

self.addEventListener('fetch', (event) => {
  event.respondWith(
    caches.match(event.request)
          .then((response) => {
            // Cache hit - return response
            if (response) {
              return response;
            }
            return fetch(event.request);
          })
  );
});

self.addEventListener('activate', (event) => {
  const cacheWhitelist = [CACHE_NAME];

  event.waitUntil(
    caches.keys().then((cacheNames) => {
      return Promise.all(
        cacheNames.map((cacheName) => {
          if (cacheWhitelist.indexOf(cacheName) === -1) {
            return caches.delete(cacheName);
          }
        })
      );
    })
  );
});

// Listen for push events and show notifications
self.addEventListener('push', function(event) {
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
    title = payload.title
    options = {
      body: payload.body,
      data: payload.data,
    }
    isJson = true;
  } catch (_) {
    try {
      const text = await event.data.text();
      let title = text;
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
self.addEventListener('notificationclick', event => {
  // Keep the SW alive while we do async work.
  event.waitUntil(handleClick(event));
});

async function handleClick(event) {
  const url = (event.notification.data && event.notification.data.url) || '/';
  event.notification.close();               // hide UI

  // 1️⃣ Try to focus an existing tab that already has the URL
  const allClients = await self.clients.matchAll({
    type: 'window',
    includeUncontrolled: true   // also see pages not yet controlled by SW
  });

  for (const client of allClients) {
    const u = new URL(client.url);
    if (u.pathname + u.search + u.hash === url) {
      return client.focus();               // bring it to front
    }
  }

  // 2️⃣ No matching tab → open a brand‑new one.
  const absoluteUrl = new URL(url, self.registration.scope).href;
  return clients.openWindow(absoluteUrl);
}
