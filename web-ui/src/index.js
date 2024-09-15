const searchInput = document.getElementById("search");
const resultList = document.getElementById("results");
const emptyState = document.getElementById("empty-state");

searchInput.addEventListener("input", async (e) => {
  const val = e.target.value;

  if (val) {
    try {
      const headers = new Headers();
      headers.append("Content-Type", "application/json");

      const response = await fetch(
        `http://localhost:2222/notes/search?query=${val.trim()}`,
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
          ]);
          hit.innerText = r.title;
          hit.addEventListener("click", (clickEvent) => {
            // TODO: Handle selected results
            console.log(`Clicked result with id ${r.id}`);
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
});
