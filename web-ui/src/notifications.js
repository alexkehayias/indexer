// Home page: opt-in for push notifications.
//
// The prompt is hidden by default and only revealed when this browser has no
// active push subscription, so already-subscribed devices are never nagged.
// Subscribing needs a user gesture (iOS in particular), hence the button.
//
// The VAPID public key is hardcoded to match the server's private key (see
// README.md). The service worker is only registered over HTTPS (register-sw.js),
// so on the plain-HTTP dev server the button can't actually subscribe.
const VAPID_PUBLIC_KEY =
  'BNKK9yweDqrtqTqUdHIhtne8YpfymNIsADbQt2ctFirKrgy1kaWu5mrPUG2F1GQAooQyVzqEa_4BnDIWzz7XRBc';

const prompt = document.getElementById('notifications-prompt');
const enableButton = document.getElementById('enable-notifications');

const supported =
  'serviceWorker' in navigator &&
  'PushManager' in window &&
  'Notification' in window;

async function currentSubscription() {
  const registration = await navigator.serviceWorker.getRegistration();
  if (!registration) return null;
  return registration.pushManager.getSubscription();
}

async function subscribe() {
  const permission = await Notification.requestPermission();
  if (permission !== 'granted') {
    console.log('Notification permission not granted');
    return false;
  }

  const registration = await navigator.serviceWorker.ready;
  const subscription = await registration.pushManager.subscribe({
    userVisibleOnly: true,
    applicationServerKey: VAPID_PUBLIC_KEY,
  });

  await fetch('/api/push/subscribe', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(subscription),
  });

  return true;
}

if (supported) {
  const subscription = await currentSubscription();
  // A denied user can't be re-prompted, so the button would do nothing.
  if (!subscription && Notification.permission !== 'denied') {
    prompt.classList.remove('hidden');
  }
}

enableButton.addEventListener('click', async () => {
  try {
    if (await subscribe()) {
      prompt.classList.add('hidden');
    }
  } catch (error) {
    console.error('Failed to subscribe the user: ', error);
  }
});
