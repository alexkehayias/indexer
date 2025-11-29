// Make loadSessions globally available for onclick handlers
window.loadSessions = async function(page = 1) {
  const sessionsList = document.getElementById('sessions-list');
  const paginationControls = document.getElementById('pagination-controls');
  const limit = 20; // Default limit

  try {
    // Fetch chat sessions from the backend with pagination
    const response = await fetch(`/notes/chat/sessions?page=${page}&limit=${limit}`, {
      method: 'GET',
      headers: {
        'Content-Type': 'application/json'
      }
    });

    if (!response.ok) {
      throw new Error(`HTTP error! status: ${response.status}`);
    }

    const data = await response.json();

    // Render the sessions
    if (data.sessions.length === 0) {
      sessionsList.innerHTML = '<p>No chat sessions found.</p>';
    } else {
      sessionsList.innerHTML = data.sessions.map(session => `
        <div class="border rounded p-4">
          <h2 class="font-semibold">Session: ${session.id}</h2>
          ${session.tags && session.tags.length > 0 ? `
            <div class="flex flex-wrap gap-2 mt-2">
              ${session.tags.map(tag => `<span class="bg-blue-100 text-blue-800 text-xs font-medium px-2.5 py-0.5 rounded">${tag}</span>`).join('')}
            </div>
          ` : ''}
          <p class="text-sm text-gray-600 mt-2">Preview: ${session.last_message_preview}</p>
          <a href="/chat/index.html?session_id=${session.id}" class="text-blue-500 hover:underline mt-2 inline-block">View Session</a>
        </div>
      `).join('');
    }

    // Render pagination controls
    renderPagination(data.page, data.limit, data.total_sessions, data.total_pages);

    // Update URL with current page
    updateURL(page);
  } catch (error) {
    console.error('Error loading sessions:', error);
    sessionsList.innerHTML = '<p>Error loading chat sessions</p>';
  }
};

function renderPagination(page, limit, totalSessions, totalPages) {
  const paginationControls = document.getElementById('pagination-controls');

  if (totalPages <= 1) {
    paginationControls.innerHTML = '';
    return;
  }

  let paginationHTML = '<div class="flex justify-center items-center space-x-2 mt-4">';

  // Previous button
  if (page > 1) {
    paginationHTML += `<button onclick="loadSessions(${page - 1})" class="px-3 py-1 border rounded hover:bg-gray-100">Previous</button>`;
  }

  // Page numbers
  const maxVisiblePages = 5;
  let startPage = Math.max(1, page - Math.floor(maxVisiblePages / 2));
  let endPage = Math.min(totalPages, startPage + maxVisiblePages - 1);

  if (endPage - startPage + 1 < maxVisiblePages) {
    startPage = Math.max(1, endPage - maxVisiblePages + 1);
  }

  for (let i = startPage; i <= endPage; i++) {
    if (i === page) {
      paginationHTML += `<span class="px-3 py-1 border rounded bg-blue-500 text-white">${i}</span>`;
    } else {
      paginationHTML += `<button onclick="loadSessions(${i})" class="px-3 py-1 border rounded hover:bg-gray-100">${i}</button>`;
    }
  }

  // Next button
  if (page < totalPages) {
    paginationHTML += `<button onclick="loadSessions(${page + 1})" class="px-3 py-1 border rounded hover:bg-gray-100">Next</button>`;
  }

  paginationHTML += '</div>';
  paginationControls.innerHTML = paginationHTML;
}

function updateURL(page) {
  const url = new URL(window.location);
  url.searchParams.set('page', page);
  window.history.replaceState({}, '', url);
}

// Load the sessions for the current page when DOM is ready
document.addEventListener('DOMContentLoaded', async () => {
  // Get page from URL parameters or default to 1
  const urlParams = new URLSearchParams(window.location.search);
  let currentPage = parseInt(urlParams.get('page')) || 1;

  // Load the sessions for the current page
  window.loadSessions(currentPage);
});