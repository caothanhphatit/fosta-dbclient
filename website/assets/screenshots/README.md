# Screenshots for the website

The landing page has screenshot slots (the table lists the main ones). Until a screenshot is added, the page shows an HTML/CSS mock of the same view, so
nothing looks broken. To use a real screenshot:

1. Save the PNG with the exact name below in this folder.
2. Add its file name to `shots.json` in this folder, e.g. `["grid.png", "diagram.png"]`.

`main.js` reads `shots.json` when the page loads and swaps the mock for each listed file (no HTML change needed). Files
that are not listed are never requested, so missing screenshots cause no 404s.

| File             | Section on the page                 | What to show                                                                                                                          |
| ---------------- | ----------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------- |
| `workspaces.png` | Workspaces and startup tasks        | The rail with 3–4 coloured workspaces, one active, and the startup task panel with a running `kubectl port-forward` and its live log. |
| `grid.png`       | A data grid made for editing        | A table with type badges, a column filter in use, one edited cell, one new row, and the pending-changes bar with Discard / Apply.     |
| `diagram.png`    | Interactive ER diagrams             | The ER diagram of a small sample database (e.g. the Docker sample data) with foreign-key links, auto-laid-out.                        |
| `assistant.png`  | An AI assistant that never executes | The assistant panel with a question, a reply containing a SQL code block, and the Insert / New tab buttons visible.                   |
| `mongo.png`      | MongoDB collection workspace       | A collection open in the workspace: section tabs, the query bar with a filter, and documents in the Table view.                     |

Guidelines:

- Use sample data only (the Docker test databases from `dev/` or the `npm run dev` mock). No real hostnames, customer data,
  keys or connection strings.
- Crop to the relevant part of the window, about 16:10 (for example 1600×1000), at 2× scale for sharp text.
- Each screenshot has a dark file (`grid.png`) and a light twin (`grid-light.png`), both listed in `shots.json`. `main.js` loads the one that matches the active theme and swaps it live when the visitor toggles the theme. Without a `-light` file the dark one is used in both themes.
- Optimise the files (e.g. `oxipng` or `pngquant`); aim for under 300 KB each.
- The alt text lives in `index.html` (`data-shot-alt` on each `<figure>`) and, translated, in `website/i18n/*.json` (keys `*.shotAlt`); update both if a screenshot shows something different.

The hero image at the top of the page is an HTML mock of the whole window and stays as is.
