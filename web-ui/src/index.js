(async function() {
  const searchInput = document.getElementById("search");
  const resultList = document.getElementById("results");
  const emptyState = document.getElementById("empty-state");

  const handleSearch = async (val) => {
    try {
      // Auto hide results from journal entries
      const query = `-title:journal ${val.trim()}`
      const headers = new Headers();
      headers.append("Content-Type", "application/json");

      const response = await fetch(
        `/notes/search?query=${query}`,
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
          hit.innerText = r.title;

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
                  id: r.id[0],
                  file_name: r.file_name[0],
                  title: r.title[0],
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
  if (initQuery) {
    searchInput.value = initQuery;
    handleSearch(initQuery);
  }

  // Handle search as you type
  searchInput.addEventListener("input", async (e) => {
    const val = e.target.value;

    if (val) {
      await handleSearch(val);
    }
  });
})();
