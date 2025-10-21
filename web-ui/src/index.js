(async function() {
  const searchInput = document.getElementById("search");
  const resultList = document.getElementById("results");
  const emptyState = document.getElementById("empty-state");

  const handleSearch = async (includeSimilarity, viewSelected, val) => {
    try {
      // Auto hide results from journal entries
      const query = `-title:journal ${encodeURIComponent(val.trim())}`
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
              case "waiting":
                taskIconContainer.innerText = "⏳";
                break;
              case "canceled":
                taskIconContainer.innerText = "❌";
                break;
              case "done":
                taskIconContainer.innerText = "✅";
                break;
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
                "text-gray-800",
                "text-xs",
                "px-2",
                "py-1",
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

            // Redirect to view the note
            if (viewSelected) {
              window.location.href = `/notes/${r.id}/view`;
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
})();
