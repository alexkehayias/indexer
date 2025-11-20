document.addEventListener('DOMContentLoaded', async () => {
  const sessionsList = document.getElementById('sessions-list');

  try {
    // Fetch chat sessions from the backend
    const response = await fetch('/notes/chat/sessions', {
      method: 'GET',
      headers: {
        'Content-Type': 'application/json'
      }
    });

    if (!response.ok) {
      throw new Error(`HTTP error! status: ${response.status}`);
    }

    const sessions = await response.json();

    // Render the sessions
    if (sessions.length === 0) {
      sessionsList.innerHTML = '<p>No chat sessions found.</p>';
    } else {
      sessionsList.innerHTML = sessions.map(session => `
        <div class="border rounded p-4">
          <h2 class="font-semibold">Session: ${session.id}</h2>
          <p class="text-sm text-gray-600">Preview: ${session.last_message_preview}</p>
          <a href="/chat/index.html?session_id=${session.id}" class="text-blue-500 hover:underline mt-2 inline-block">View Session</a>
        </div>
      `).join('');
    }
  } catch (error) {
    console.error('Error loading sessions:', error);
    sessionsList.innerHTML = '<p>Error loading chat sessions</p>';
  }
});