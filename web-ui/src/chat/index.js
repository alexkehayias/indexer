import MessageBubble from './message-bubble.js';

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

          if (!isToolCall && (isUser || isAssistant)) {
            const bubble = new MessageBubble();
            bubble.setAttribute('message', message.content);
            bubble.setAttribute('is-user-message', isUser.toString());
            bubble.setAttribute('is-tool-call', isToolCall.toString());
            document.getElementById('chat-display').prepend(bubble);
          }
          if (isAssistant && isToolCall) {
            const bubble = new MessageBubble();

            let toolCallMessages = [];
            for (const t of message.tool_calls) {
              const toolFn = t.function;
              toolCallMessages.push(`**Tool call**: \`${toolFn.name}\`\n**Args**:\n\n\`\`\`\n${toolFn.arguments}\n\`\`\``);
            }

            bubble.setAttribute('message', toolCallMessages.join('\n\n'));
            bubble.setAttribute('is-user-message', 'false');
            bubble.setAttribute('is-tool-call', 'true');
            document.getElementById('chat-display').prepend(bubble);
          }
        });
      }
      scrollToBottom();
    })
    .catch(error => console.error('Error:', error));
  } else {
    sessionId = crypto.randomUUID();
    history.replaceState({}, '', `?session_id=${sessionId}`);
  }

  const chatContainer = document.getElementById('chat-container');
  const chatInput = document.getElementById('chat-input');
  const sendButton = document.getElementById('send-button');

  const scrollToBottom = () => {
    chatContainer.scrollTop = chatContainer.scrollHeight;
  };

  // Auto-resize textarea up to 7 lines
  const autoResize = () => {
    chatInput.style.height = 'auto';
    const newHeight = Math.min(chatInput.scrollHeight, 7 * 24); // Approximate line height
    chatInput.style.height = newHeight + 'px';
  };

  chatInput.addEventListener('input', autoResize);

  sendButton.addEventListener('click', () => sendMessage());
  chatInput.addEventListener('keydown', (e) => {
    if (e.metaKey && e.key === 'Enter') {
      e.preventDefault();
      sendMessage();
    };
  });

  const sendMessage = () => {
    const message = chatInput.value.trim();
    if (message === '') return;

    // Create user message bubble
    const userBubble = new MessageBubble();
    userBubble.setAttribute('message', message);
    userBubble.setAttribute('is-user-message', 'true');
    userBubble.setAttribute('is-tool-call', 'false');
    userBubble.setAttribute('is-loading', 'false');
    document.getElementById('chat-display').prepend(userBubble);

    // Create assistant message bubble
    const assistantBubble = new MessageBubble();
    assistantBubble.setAttribute('is-user-message', 'false');
    assistantBubble.setAttribute('is-tool-call', 'false');
    assistantBubble.setAttribute('is-loading', 'true');
    document.getElementById('chat-display').prepend(assistantBubble);

    scrollToBottom();

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
                  const reasoning = parsed.choices[0].delta.reasoning;
                  const toolCalls = parsed.choices[0].delta.tool_calls;
                  const toolCallsFinished = parsed.choices[0].finish_reason === 'tool_calls';

                  // TODO: Handle rendering tool calls

                  // Handle content delta
                  if (content) {
                    contentAccum += content;
                    assistantBubble.setAttribute('is-loading', 'false');
                    assistantBubble.updateContent(contentAccum);
                  }

                  // Handle reasoning delta
                  if (reasoning) {
                    // Tool call deltas are interleaved with reasoning
                    // deltas as the model thinks through the tools to
                    // use and retrieves the results so we wait until
                    // it's dont to render the message bubble.
                    assistantBubble.setAttribute('is-loading', 'false');
                    // FIX: There is no div to add this to because it
                    // might still be in the loading state.
                    // Need to refactor reasoning to appear in a message bubble
                    assistantBubble.addReasoning(reasoning);
                  }
                } catch (e) {
                  console.error('Error parsing JSON:', e);
                }
              }
            });

            read();
          }).catch(error => {
            console.error('Read error:', error);
          });
        }

        read();
      });

    chatInput.value = ''; // Clear input field
    chatInput.style.height = 'auto'; // Reset height after sending
  };
});
