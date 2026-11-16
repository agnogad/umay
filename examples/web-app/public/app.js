document.addEventListener("DOMContentLoaded", () => {
  const form = document.getElementById("note-form");
  const input = document.getElementById("note-text");
  const err = document.getElementById("note-error");

  if (form && input) {
    form.addEventListener("submit", async (e) => {
      e.preventDefault();
      if (err) err.textContent = "";
      const body = new URLSearchParams({ text: input.value });
      const res = await fetch("/api/notes", {
        method: "POST",
        headers: { "Content-Type": "application/x-www-form-urlencoded" },
        body,
      });
      if (res.ok) {
        window.location.reload();
        return;
      }
      const data = await res.json().catch(() => ({}));
      if (err) err.textContent = data.error || "Could not create note.";
    });
  }

  document.querySelectorAll(".delete-note").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const id = btn.getAttribute("data-id");
      if (!id) return;
      const res = await fetch(`/api/notes/${encodeURIComponent(id)}`, {
        method: "DELETE",
      });
      if (res.ok) {
        window.location.reload();
      }
    });
  });
});
