// Stands in for a bundler's fingerprinted entry chunk: renders the client-side
// route and calls the API.
const app = document.getElementById("app");
app.textContent = `client route: ${location.pathname}`;
fetch("/api/hello")
  .then((response) => response.text())
  .then((text) => app.append(` · api says: ${text}`));
