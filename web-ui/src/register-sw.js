// Registers the service worker for the PWA. Loaded by the home page
// (/index.html) so the worker is installed on first visit and controls the
// whole app scope (/). Include via:
//   <script src="/register-sw.js"></script>
//
// Only registered over HTTPS (production). Dev is served over plain HTTP, so
// keeping the worker out of dev means reloads always hit the network instead
// of serving cached assets. Any worker left over from a previous session is
// unregistered so it stops controlling dev pages.
if ('serviceWorker' in navigator) {
  window.addEventListener('load', async () => {
    if (window.location.protocol !== 'https:') {
      const registrations = await navigator.serviceWorker.getRegistrations();
      await Promise.all(
        registrations.map((registration) => registration.unregister()),
      );
      return;
    }

    navigator.serviceWorker.register('/service-worker.js').catch((error) => {
      console.error('Service worker registration failed:', error);
    });
  });
}
