document.addEventListener('DOMContentLoaded', () => {
  // Preload dog images to avoid fetching them each time
  const dogImages = [];
  for (let i = 1; i <= 3; i++) {
    const img = new Image();
    img.src = `./img/dog${i}.png`;
    dogImages.push(img);
  }

  const urlParams = new URLSearchParams(window.location.search);
  let sessionId;
  const maybeSessionId = urlParams.get("session_id");
  if (maybeSessionId) {
    sessionId = maybeSessionId;
    fetch(`/notes/chat/${sessionId}`, {
      method: 'GET',
      headers: {
        'Content-Type': 'application/json'
      },
    })
    .then(response => {
      if (response.status === 404) {
        console.log('Session not found, starting new conversation');
        return Promise.resolve(null);
      } else if (!response.ok) {
        throw new Error(`HTTP error! status: ${response.status}`);
      }
      return response.json();
    })
    .then(data => {
      // Only process transcript if we have data
      if (data && data.transcript) {
        data.transcript.map(message => {
          const isUser = message.role === 'user';
          const isAssistant = message.role === 'assistant';
          const isSystem = message.role === 'system';
          const isToolCall = (message.role === 'tool') || (isAssistant && !message.content);

          if (!isSystem) {
            renderMessageBubble(message.content, isUser, isToolCall);
          }
        });
      }
    })
    .catch(error => console.error('Error:', error));
  } else {
    sessionId = crypto.randomUUID();
    history.replaceState({}, '', `?session_id=${sessionId}`);
  }

  const chatContainer = document.getElementById('chat-container');
  const chatDisplay = document.getElementById('chat-display');
  const chatInput = document.getElementById('chat-input');
  const sendButton = document.getElementById('send-button');

  sendButton.addEventListener('click', () => sendMessage());
  chatInput.addEventListener('keypress', (e) => {
    if (e.key === 'Enter') {
      e.preventDefault();
      sendMessage();
    };
  });

  const renderMessageBubble = (message, isUserMessage, isToolCall, isLoading = false) => {
    // Empty messages mean this was a tool call or a response to a //
    // tool call so skip this for now. Maybe later will render some debug
    // info for inspecting tool calls.
    if (isToolCall) {
      return
    };

    const messageElement = document.createElement('div');
    messageElement.className = isLoading ? 'flex justify-center my-4' : 'flex items-start gap-2.5 mb-4';

    if (isLoading) {
      for (let i = 0; i < 3; i++) {
        const img = document.createElement('img');
        img.src = `./img/dog${i+1}.png`;  // Placeholder - update with actual dog image paths
        img.className = `w-8 h-8 animate-bounce-dog${i+1}`;
        img.alt = 'Loading Dog';
        messageElement.appendChild(img);
      }
    } else {
      const imgElement = document.createElement('img');
      imgElement.className = 'w-8 h-8 rounded-full';
      imgElement.src = isUserMessage ? './img/me.jpeg' : './img/bot.jpeg';
      imgElement.alt = isUserMessage ? 'User image' : 'Bot image';

      const messageContent = document.createElement('div');
      messageContent.className = 'flex flex-col gap-1 w-[320px] lg:w-full overflow-auto';

      const messageBody = document.createElement('div');
      messageBody.className = isUserMessage
        ? 'flex flex-col leading-1.5 p-4 bg-blue-100 rounded-e-xl rounded-es-xl'
        : 'flex flex-col leading-1.5 p-4 border-gray-200 bg-gray-100 rounded-e-xl rounded-es-xl';

      const messageHTML = marked.parse(message, { breaks: true });

      const messageText = document.createElement('div');
      messageText.className = 'markdown overflow-auto text-sm lg:text-base font-normal';
      messageText.innerHTML = messageHTML;
      messageBody.appendChild(messageText);
      messageContent.appendChild(messageBody);
      messageElement.appendChild(imgElement);
      messageElement.appendChild(messageContent);
    }

    chatDisplay.prepend(messageElement);
    chatContainer.scrollTop = chatContainer.scrollHeight;

    return messageElement;
  };

  const sendMessage = () => {
    const message = chatInput.value.trim();
    if (message === '') return;

    renderMessageBubble(message, true);

    // Show loading indicator below user's message
    const loadingElement = renderMessageBubble('', false, false, true);

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
      // Responses to a user message will never be tool calls
      renderMessageBubble(data.message, false, false, false);
      // Remove loading indicator
      loadingElement.remove();
    })
    .catch(error => {
      console.error('Error:', error);
      loadingElement.remove(); // Ensure loading indicator is removed on error
    });

    chatInput.value = ''; // Clear input field
  };
});

// CSS styles for loading animation
const style = document.createElement('style');
style.innerHTML = `
.animate-bounce-dog1 {
  animation: bounce 1s infinite 0.2s;
}
.animate-bounce-dog2 {
  animation: bounce 1s infinite 0.4s;
}
.animate-bounce-dog3 {
  animation: bounce 1s infinite 0.6s;
}
@keyframes bounce {
  0%, 100% {
    transform: translateY(0);
  }
  50% {
    transform: translateY(-10px);
  }
}
`;
document.head.appendChild(style);
