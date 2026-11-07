# Screenshots for the website

The landing page has four screenshot slots. Until a screenshot is added, the page shows an HTML/CSS mock of the same view, so
nothing looks broken. To use a real screenshot:

1. Save the PNG with the exact name below in this folder.
2. Add its file name to `shots.json` in this folder, e.g. `["grid.png", "diagram.png"]`.

`main.js` reads `shots.json` when the page loads and swaps the mock for each listed file (no HTML change needed). Files
that are not listed are never requested, so missing screenshots cause no 404s.

| File             | Section on the page                 | What to show                                                                                                                            |
| ---------------- | ----------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------- |
| `workspaces.png` | Workspaces and startup tasks        | The rail with 3–4 coloured workspaces, one active, and the startup task panel with a running `kubectl port-forward` and its live log.   |
| `grid.png`       | A data grid made for editing        | A table with type badges, a column filter in use, one edited cell, one new row, and the pending-changes bar with Discard / Apply.      |
| `diagram.png`    | Interactive ER diagrams             | The ER diagram of a small sample database (e.g. the Docker sample data) with foreign-key links, auto-laid-out.                         |
| `assistant.png`  | An AI assistant that never executes | The assistant panel with a question, a reply containing a SQL code block, and the Insert / New tab buttons visible.                     |

Guidelines:

- Use sample data only (the Docker test databases from `dev/` or the `npm run dev` mock). No real hostnames, customer data,
  keys or connection strings.
- Crop to the relevant part of the window, about 16:10 (for example 1600×1000), at 2× scale for sharp text.
- Light theme fits both page themes best; keep all four in the same theme.
- Optimise the files (e.g. `oxipng` or `pngquant`); aim for under 300 KB each.
- The alt text lives in `index.html` (`data-shot-alt` on each `<figure>`); update it if a screenshot shows something different.

The hero image at the top of the page is an HTML mock of the whole window and stays as is.
