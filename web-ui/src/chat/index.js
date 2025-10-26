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

      // Add syntax highlighting to code blocks
      const tempDiv = document.createElement('div');
      tempDiv.innerHTML = messageHTML;
      tempDiv.querySelectorAll('pre code').forEach((block) => {
        hljs.highlightElement(block);
      });
      const highlightedHTML = tempDiv.innerHTML;

      const messageText = document.createElement('div');
      messageText.className = 'markdown overflow-auto text-sm lg:text-base font-normal';
      messageText.innerHTML = highlightedHTML;
      messageBody.appendChild(messageText);
      messageContent.appendChild(messageBody);
      messageElement.appendChild(imgElement);
      messageElement.appendChild(messageContent);

      // Add methods for streaming updates to the content.
      messageElement.updateContent = function(txt) {
        const updatedHTML = marked.parse(txt, { breaks: true });
        
        // Add syntax highlighting to code blocks
        const tempDiv = document.createElement('div');
        tempDiv.innerHTML = updatedHTML;
        tempDiv.querySelectorAll('pre code').forEach((block) => {
          hljs.highlightElement(block);
        });
        messageText.innerHTML = tempDiv.innerHTML;
      }
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
      .then(response => {
        const reader = response.body.getReader();
        const decoder = new TextDecoder();
        let buffer = '';

        let messageBubbleEl = renderMessageBubble('', false, false, false);
        let contentAccum = '';

        function read() {
          reader.read().then(({done, value}) => {
            if (done) {
              console.log('Stream complete');
              return;
            }

            // Convert Uint8Array to string
            const chunk = decoder.decode(value, {stream: true});
            buffer += chunk;

            // Process complete lines
            const lines = buffer.split('\n');
            buffer = lines.pop(); // Keep incomplete line in buffer

            lines.forEach(line => {
              if (line.startsWith('data: ')) {
                const data = line.slice(6).trim();
                if (data === '[DONE]') {
                  console.log('Stream finished');
                  return;
                }
                try {
                  const parsed = JSON.parse(data);
                  const content = parsed.choices[0].delta.content;

                  // Handle content delta
                  if (content) {
                    loadingElement.remove();
                    contentAccum += content;
                    messageBubbleEl.updateContent(contentAccum);
                  }

                  // TODO: Handle other kinds of deltas
                } catch (e) {
                  console.error('Error parsing JSON:', e);
                }
              }
            });

            read();
          }).catch(error => {
            console.error('Read error:', error);
            loadingElement.remove(); // Ensure loading indicator is removed on error
          });
        }

        read();
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
