document.addEventListener('DOMContentLoaded', () => {
  const urlParams = new URLSearchParams(window.location.search);
  const sessionId = urlParams.get("session_id") || crypto.randomUUID();
  history.replaceState({}, '', `?session_id=${sessionId}`);

  const chatDisplay = document.getElementById('chat-display');
  const chatInput = document.getElementById('chat-input');
  const sendButton = document.getElementById('send-button');

  sendButton.addEventListener('click', () => sendMessage());
  chatInput.addEventListener('keypress', (e) => {
    if (e.key === 'Enter') sendMessage();
  });

  const renderMessageBubble = (message, isUserMessage) => {
    const messageElement = document.createElement('div');
    messageElement.className = 'flex items-start gap-2.5 mb-4';

    const imgElement = document.createElement('img');
    imgElement.className = 'w-8 h-8 rounded-full';
    imgElement.src = isUserMessage
                   ? './img/me.jpeg'
                   : './img/bot.jpeg';
    imgElement.alt = isUserMessage ? 'User image' : 'Bot image';

    const messageContent = document.createElement('div');
    messageContent.className = 'flex flex-col gap-1 w-full max-w-[320px]';

    const messageBody = document.createElement('div');
    messageBody.className = isUserMessage
      ? 'flex flex-col leading-1.5 p-4 bg-blue-500 text-white rounded-e-xl rounded-es-xl'
      : 'flex flex-col leading-1.5 p-4 border-gray-200 bg-gray-100 text-gray-900 dark:bg-gray-700 rounded-e-xl rounded-es-xl';

    // Convert Markdown to HTML using marked
    const messageHTML = marked.parse(message);

    const messageText = document.createElement('p');
    messageText.className = 'text-sm font-normal';
    messageText.innerHTML = messageHTML;
    messageBody.appendChild(messageText);
    messageContent.appendChild(messageBody);
    messageElement.appendChild(imgElement);
    messageElement.appendChild(messageContent);

    // Prepend since we use `flex-direction: column-reverse` to render
    // the chat messages from bottom to top.
    chatDisplay.prepend(messageElement);
    // Scroll to the bottom of the chat
    chatDisplay.scrollTop = chatDisplay.scrollHeight;
  };

  const sendMessage = () => {
    const message = chatInput.value.trim();
    if (message === '') return;

    // Render user's message immediately
    renderMessageBubble(message, true);

    const chatRequest = {
      session_id: sessionId,
      message: message
    };

    fetch('/notes/chat', {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json'
      },
      body: JSON.stringify(chatRequest)
    })
    .then(response => response.json())
    .then(data => {
      // Handle the new response structure
      renderMessageBubble(data.message, false);
    })
    .catch(error => console.error('Error:', error));

    chatInput.value = ''; // Clear input field
  };
});
