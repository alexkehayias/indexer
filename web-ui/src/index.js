const searchInput = document.getElementById("search");
searchInput.addEventListener("input", async (e) => {
  const val = e.target.value;

  if (val) {
    console.log("Searching:", val);
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

      const json = await response.json();
      console.log(json);
    } catch (error) {
      console.error("Server error", error.message);
    }
  }
});
