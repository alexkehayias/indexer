(async function() {
  const searchInput = document.getElementById("search");
  const resultList = document.getElementById("results");
  const emptyState = document.getElementById("empty-state");

  const handleSearch = async (includeSimilarity, viewSelected, val) => {
    try {
      // Auto hide results from journal entries
      const query = encodeURIComponent("-title:journal " + val);
      const headers = new Headers();
      headers.append("Content-Type", "application/json");

      const response = await fetch(
        `/notes/search?query=${query}&include_similarity=${includeSimilarity}`,
        {
          method: "GET",
          headers,
        }
      );
      if (!response.ok) {
        throw new Error(`Error fetching: ${response.status}`);
      }

      const data = await response.json();

      if (data.results.length > 0) {
        emptyState.classList.add("hidden");
        resultList.classList.remove("hidden");

        const hits = data.results.map((r) => {
          // Create a list item for each hit
          const hit = document.createElement("li");
          hit.classList.add(...[
            "group",
            "flex",
            "justify-between",
            "cursor-default",
            "select-none",
            "items-center",
            "rounded-md",
            "px-3",
            "py-2",
            "hover:cursor-pointer",
          ]);

          const titleContainer = document.createElement("div");
          titleContainer.classList.add(...[
            "flex",
            "space-x-2",
          ]);

          // If this is a task, show a todo icon
          if (r.is_task) {
            const taskIconContainer = document.createElement("span");
            taskIconContainer.classList.add(...[
              "py-0.5",
              "text-gray-800",
              "text-xs",
              "rounded-full",
            ]);
            // Map the status to an icon
            switch (r.task_status.toLowerCase()) {
              case "todo":
                taskIconContainer.innerText = "⬜";
                break;
              case "next":
                taskIconContainer.innerText = "⏭️";
                break;
              case "waiting":
                taskIconContainer.innerText = "⏳";
                break;
              case "canceled":
                taskIconContainer.innerText = "❌";
                break;
              case "done":
                taskIconContainer.innerText = "✅";
                break;
              case "someday":
                taskIconContainer.innerText = "🤷";
              default:
                break;
            }
            titleContainer.appendChild(taskIconContainer);
          }

          // Add in the title
          const titleTextContainer = document.createElement("span");
          titleTextContainer.classList.add(...[
            "line-clamp-1",
          ]);
          titleTextContainer.innerText = r.title;
          titleContainer.appendChild(titleTextContainer);

          hit.appendChild(titleContainer);

          // Add in each tag
          // Tags are a comma separated string so we need to check if
          // there is an empty string to determine if there are any tags
          // to render
          if (r.tags) {
            const tagContainer = document.createElement("div");
            tagContainer.classList.add(...["flex", "flex-row"]);
            r.tags.split(",").forEach((tag) => {
              const tagDiv = document.createElement("div");
              tagDiv.classList.add(...[
                "bg-gray-200",
                "text-gray-700",
                "text-xs",
                "px-2",
                "py-0.5",
                "rounded-full",
                "mr-2",
              ]);
              tagDiv.innerText = `#${tag}`;
              tagContainer.appendChild(tagDiv);
            });
            hit.appendChild(tagContainer);
          }

          hit.addEventListener("click", async (clickEvent) => {
            console.log(`Clicked result with id ${r.id}`);

            // Unselect all other hits
            hits.forEach((hit) => {
              hit.classList.remove(...["bg-blue-700", "text-white"]);
            });

            // Highlight the selected hit
            hit.classList.add(...["bg-blue-700", "text-white"]);

            // Store the selected hit in the search session
            const resp = await fetch(
              `/notes/search/latest`,
              {
                method: "POST",
                body: JSON.stringify({
                  id: r.id,
                  file_name: r.file_name,
                  title: r.title,
                }),
                headers: {
                  'Accept': 'application/json',
                  'Content-Type': 'application/json'
                },
              }
            );
            if (!resp.ok) {
              throw new Error(`Error updating latest hit: ${response.status}`);
            } else {
              console.log(`Updated latest hit to ${r.id}`)
            }

            // Show note in fullscreen modal if viewSelected
            if (viewSelected) {
              // Create or reuse overlay modal
              let modal = document.getElementById('note-modal');
              let addedModal = false;
              if (!modal) {
                modal = document.createElement('div');
                modal.id = 'note-modal';
                modal.className = 'fixed inset-0 flex items-center justify-center bg-black bg-opacity-85 z-[10000]';
                modal.innerHTML = `<div id=\"note-modal-content\" class=\"relative bg-white max-w-2xl w-[95vw] max-h-[90vh] rounded-lg shadow-xl p-8 overflow-auto\"></div>`;
                document.body.appendChild(modal);
                addedModal = true;
              }
              const content = modal.querySelector('#note-modal-content');

              // Show loading
              content.innerHTML = '<div class="mb-4 text-center text-xl">Loading...</div>';
              modal.style.display = 'flex';

              function dismissModal() {
                modal.style.display = 'none';
                document.removeEventListener('keydown', escListener);
                if (addedModal) {
                  modal.remove();
                }
              }

              // Click outside the modal content to close
              modal.onclick = function(e) {
                if (e.target === modal) {
                  dismissModal();
                }
              }

              // ESC key closes modal
              function escListener(e) {
                if (e.key === 'Escape') {
                  dismissModal();
                }
              }
              document.addEventListener('keydown', escListener);

              // Make modal closable via button
              if (!document.getElementById('modal-close-btn')) {
                const closeBtn = document.createElement('button');
                closeBtn.id = 'modal-close-btn';
                closeBtn.innerText = '×';
                closeBtn.className = 'absolute top-3 right-6 bg-transparent border-0 text-3xl text-gray-500 hover:text-black cursor-pointer';
                closeBtn.onclick = dismissModal;
                content.appendChild(closeBtn);
              }

              // Fetch and render the note JSON
              fetch(`/notes/${r.id}/view`,{
                  headers: { 'Accept': 'application/json' }
                })
                .then(async(resp) => {
                  if (!resp.ok) throw new Error('Failed to fetch note');
                  return resp.json();
                })
                .then(noteData => {
                  let html = '';
                  // Render markdown into HTML
                  const messageHtml = marked.parse(noteData.body, { breaks: true });
                  // Tags
                  if (noteData.tags) {
                    html += `<div class=\"mb-4\">${noteData.tags.split(',').map(t=>`<span class=\"inline-block mr-2 bg-gray-200 text-gray-700 text-xs px-2 py-0.5 rounded-full\">#${t}</span>`).join('')}</div>`;
                  }
                  // Content
                  html += `<div class=\"markdown leading-relaxed text-base text-gray-800\">${messageHtml||''}</div>`;
                  // Insert and keep the close button on top
                  content.innerHTML = `<button id=\"modal-close-btn\" class=\"absolute top-3 right-6 bg-transparent border-0 text-3xl text-gray-500 hover:text-black cursor-pointer\">×</button>` + html;
                  content.querySelector('#modal-close-btn').onclick = dismissModal;
                })
                .catch(err => {
                  content.innerHTML = `<div class=\"text-center text-red-700 p-8\">Failed to load note: ${err.message}</div>`;
                })
              ;
              return;
            }
          })
          return hit;
        })
        resultList.replaceChildren(...hits);
      } else {
        resultList.classList.add("hidden");
        emptyState.classList.remove("hidden");
      }
    } catch (error) {
      console.error("Server error", error.message);
    }
  }

  // If there is already a query, initiate the search
  const urlParams = new URLSearchParams(window.location.search);
  const initQuery = urlParams.get("query");
  const includeSimilarity = urlParams.get("include_similarity") === "true";
  const viewSelected = urlParams.get("view_selected") === "true";

  if (initQuery) {
    searchInput.value = initQuery;
    handleSearch(includeSimilarity, viewSelected, initQuery);
  }

  // Handle search as you type
  searchInput.addEventListener("input", async (e) => {
    const val = e.target.value;

    if (val) {
      await handleSearch(includeSimilarity, viewSelected, val);
    }
  });

  // Register the service worker
  if ('serviceWorker' in navigator) {
    window.addEventListener('load', () => {
      navigator.serviceWorker.register('/service-worker.js').then(registration => {
        console.log('SW registered: ', registration);
      }).catch(registrationError => {
        console.log('SW registration failed: ', registrationError);
      });
    });
  }

  // Function to detect mobile Safari
  const isMobileSafari = () => {
    return /iP(ad|hone|od).+Version\/[\d\.]+.*Safari/i.test(navigator.userAgent);
  }

  const subscribeToPushNotifications = async () => {
    try {
      const permission = await Notification.requestPermission();
      if (permission !== 'granted') {
        console.log('Notification permission not granted');
        return;
      }

      // Subscribe to the Push service
      const registration = await navigator.serviceWorker.ready;
      const subscription = await registration.pushManager.subscribe({
        userVisibleOnly: true,
        applicationServerKey: 'BNKK9yweDqrtqTqUdHIhtne8YpfymNIsADbQt2ctFirKrgy1kaWu5mrPUG2F1GQAooQyVzqEa_4BnDIWzz7XRBc'
      });

      // Send subscription to server
      await fetch('/push/subscribe', {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json'
        },
        body: JSON.stringify(subscription)
      });
    } catch (error) {
      console.error('Failed to subscribe the user: ', error);
    }
  }

  // Show push notification permission button if on mobile Safari
  if (isMobileSafari() && 'Notification' in window && navigator.serviceWorker) {
    const permissionButton = document.createElement('button');
    permissionButton.innerText = 'Enable Notifications';
    permissionButton.classList.add(...[
      "fixed",
      "z-10",
      "bottom-10",
      "right-10",
      "rounded-md",
      "bg-white",
      "px-2.5",
      "py-1.5",
      "text-sm",
      "font-semibold",
      "text-gray-900",
      "shadow-sm",
      "ring-1",
      "ring-inset",
      "ring-gray-300",
      "hover:bg-gray-50",
      "hover:cursor-pointer",
    ]);

    document.body.appendChild(permissionButton);

    permissionButton.addEventListener('click', async function() {
      try {
        await subscribeToPushNotifications();
        permissionButton.style.display = 'none';
      } catch (error) {
        console.error('Failed to subscribe the user: ', error);
      }
    });
  } else {
    await subscribeToPushNotifications();
  }

})();
