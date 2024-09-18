(async function() {
  const searchInput = document.getElementById("search");
  const resultList = document.getElementById("results");
  const emptyState = document.getElementById("empty-state");

  const handleSearch = async (val) => {
    try {
      const headers = new Headers();
      headers.append("Content-Type", "application/json");

      const response = await fetch(
        `/notes/search?query=${val.trim()}`,
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
          const hit = document.createElement("li");
          hit.classList.add(...[
            "group",
            "flex",
            "cursor-default",
            "select-none",
            "items-center",
            "rounded-md",
            "px-3",
            "py-2",
            "hover:cursor-pointer",
          ]);
          hit.innerText = r.title;
          hit.addEventListener("click", async (clickEvent) => {
            console.log(`Clicked result with id ${r.id}`);
            hit.classList.add(...["bg-indigo-700", "text-white"]);
            const resp = await fetch(
              `/notes/search/latest`,
              {
                method: "POST",
                body: JSON.stringify({
                  id: r.id[0],
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
