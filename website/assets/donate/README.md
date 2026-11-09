# Donation QR codes

The Support page (`/donate/`, template `website/donate.html`) has two QR code slots. Until a code is added, each slot shows a
dashed box with "QR code coming soon", so nothing looks broken. `main.js` reads `qr.json` in this folder and only requests the
files listed there, so a missing file never causes a 404.

| File          | Slot on the page       | What to put in it                                                  |
| ------------- | ---------------------- | ------------------------------------------------------------------ |
| `bank.png`    | Bank transfer (VietQR) | The VietQR code of the bank account that receives donations.       |
| `ewallet.png` | E-wallet               | The QR code of the e-wallet account (for example MoMo or ZaloPay). |

## Enable a slot

1. Save the PNG with the exact name above in this folder. Square, at least 480×480 px, with the quiet zone (white border)
   the bank or wallet app exports; keep it under 100 KB. The page shows it on white in both themes.
2. List its file name in `qr.json`, e.g. `["bank.png"]` or `["bank.png", "ewallet.png"]`.
3. Replace the placeholder texts under the slot, in **all four** language files (`website/i18n/en.json`, `vi.json`,
   `zh.json`, `ja.json`) and, for English, also in `website/donate.html` (the build fails if the two differ):
   - `don.qr.bank.details` ("Account details coming soon."): for example the bank name and the account holder's name;
   - `don.qr.wallet.details` ("Wallet details coming soon."): for example the wallet name and the account name;
   - optionally the titles `don.qr.bank.title` / `don.qr.wallet.title` and the image descriptions `don.qr.bank.alt` /
     `don.qr.wallet.alt` (read by screen readers).
4. Run `node website/build.mjs --check`, preview the page (`node website/build.mjs .site`), and scan each code from the
   preview with a phone before publishing.

The details strings accept the same inline markup as the other strings (`<b>`, `<code>`, `<br>`, `https://` links). Do not put
anything in them you would not print on a public page.

To remove a slot, take its name out of `qr.json` (the placeholder comes back) or delete its `<li>` in `donate.html`.
