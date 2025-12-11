class MessageBubble extends HTMLElement {
  constructor() {
    super();

    // Initialize state
    this.isToolCall = false;
    this.isLoading = false;
    this.isUserMessage = false;
  }

  static get observedAttributes() {
    return ['message', 'is-user-message', 'is-tool-call', 'is-loading'];
  }

  attributeChangedCallback(name, oldValue, newValue) {
    if (oldValue !== newValue) {
      switch (name) {
        case 'message':
          this.message = newValue;
          break;
        case 'is-user-message':
          this.isUserMessage = newValue === 'true';
          break;
        case 'is-tool-call':
          this.isToolCall = newValue === 'true';
          break;
        case 'is-loading':
          this.isLoading = newValue === 'true';
          break;
      }
      this.render();
    }
  }

  connectedCallback() {
    this.render();
  }

  render() {
    if (this.isLoading) {
      this.innerHTML = `
        <div class="flex justify-center my-4">
          <img src="./img/dog1.png" class="w-8 h-8 animate-bounce" alt="Loading Dog">
          <img src="./img/dog2.png" class="w-8 h-8 animate-bounce" alt="Loading Dog">
          <img src="./img/dog3.png" class="w-8 h-8 animate-bounce" alt="Loading Dog">
        </div>
      `;
    } else {
      const imgSrc = this.isUserMessage ? './img/me.jpeg' : './img/bot.jpeg';
      const messageBodyClass = this.isUserMessage
        ? 'flex flex-col leading-1.5 p-4 bg-blue-100 rounded-xl'
        : 'flex flex-col leading-1.5 p-4 border-gray-200 bg-gray-100 rounded-xl';

      this.innerHTML = `
        <div class="flex items-start gap-2.5 mb-4">
          <img src="${imgSrc}" class="hidden w-8 h-8 rounded-full md:block" alt="${this.isUserMessage ? 'User image' : 'Bot image'}">
          <div class="flex flex-col gap-1 w-full overflow-auto">
            <div class="${messageBodyClass}">
              <div class="markdown overflow-auto text-sm lg:text-base font-normal"></div>
            </div>
          </div>
        </div>
      `;

      // Update content if message is provided
      if (this.message) {
        this.updateContent(this.message);
      }
    }

    // Scroll to bottom when message is added
    this.scrollToBottom();
  }

  scrollToBottom() {
    // This method will be called when the component is added to DOM
    // We can't directly access parent here, so we'll rely on the parent logic
  }

  updateContent(message) {
    if (this.isLoading) return;

    const messageTextElement = this.querySelector('.markdown');
    if (messageTextElement) {
      // Parse markdown
      const messageHTML = marked.parse(message, { breaks: true });

      // Add syntax highlighting to code blocks
      const tempDiv = document.createElement('div');
      tempDiv.innerHTML = messageHTML;
      tempDiv.querySelectorAll('pre code').forEach((block) => {
        hljs.highlightElement(block);
      });

      messageTextElement.innerHTML = tempDiv.innerHTML;
    }
  }

  // Method to add reasoning section
  addReasoning(reasoningContent) {
    if (this.isLoading || this.isToolCall) return;

    // Create reasoning container if it doesn't exist
    let reasoningContainer = this.querySelector('details');

    if (!reasoningContainer) {
      reasoningContainer = document.createElement('details');
      reasoningContainer.className = 'mb-1 cursor-pointer list-none rounded-xl bg-white border pl-3 py-2';
      reasoningContainer.innerHTML = `
          <summary class="font-semibold">Thinking...</summary>
      `;

      const reasoningContentElement = document.createElement('div');
      reasoningContentElement.className = 'text-sm text-gray-700 pl-4';
      reasoningContainer.appendChild(reasoningContentElement);

      // Insert at the beginning of message content (before other elements)
      const messageContent = this.querySelector('.flex.flex-col.gap-1.w-full.overflow-auto');
      if (messageContent && messageContent.firstChild) {
        messageContent.insertBefore(reasoningContainer, messageContent.firstChild);
      } else {
        messageContent.appendChild(reasoningContainer);
      }
    }

    // Update reasoning content
    const contentElement = reasoningContainer.querySelector('.text-sm.text-gray-700.pl-4');
    if (contentElement) {
      contentElement.textContent += reasoningContent;
    }
  }
}

// Define the custom element
customElements.define('message-bubble', MessageBubble);

export default MessageBubble;
