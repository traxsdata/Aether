# Data loss prevention and file encryption

æther protects your files in two layers that work without getting in your way.

## 1. Encryption at rest

The first time æther boots on a **blank disk**, it asks you to choose a
passphrase and encrypts the whole disk with it. After that, every boot shows
the unlock screen. That's the only question it asks.

- Your passphrase goes through PBKDF2-HMAC-SHA256 (200,000 rounds, random
  salt) to make a key-encryption key. That key unwraps a random 256-bit
  volume key, which never leaves memory unwrapped.
- Every block except the superblock and free-space bitmap is sealed with
  **ChaCha20-Poly1305**: file contents, folder listings (so file names too),
  the inode table and the block maps. Each file has its own key, derived
  with HKDF.
- Each block's tag also covers *which file and which position* it belongs
  to. Moving, swapping or editing blocks on the disk is detected, and the
  read fails with "failed integrity check" instead of returning bad data.
- æther never formats a disk that holds anything it doesn't recognise. With
  no blank or æther disk it uses **temporary storage** in memory and says so
  in the top bar.
- All of the cryptography is written from the RFCs for æther and checked
  against the official test vectors at every boot. If the check fails, æther
  refuses to store files.

## 2. Labels that follow your data

Every file has a label: **Public**, **Personal**, **Confidential** or
**Secret**. Every running app has one too: the highest label of anything it
has read. The title bar shows it, drawn by the kernel so the app can't hide it.

| Rule | What it means |
|------|---------------|
| Labels follow the data | An app holding Confidential data that saves a file makes that file Confidential. Nothing to click, nothing to remember. |
| New files inherit | A new file gets the higher of its folder's label and the app's label. Files in your Home start as Personal. |
| Lowering always asks | Only you can lower a label, in a system dialog the kernel draws. "Keep" is the default, and an app can't click it for you. After 3 refusals an app can't ask again. |
| Leaving is checked | Data leaving the protected store is checked against the app's label. Today the kernel log is the only way out, and Confidential or Secret data is refused there. Network, USB and printing will use the same check when they exist. |
| Everything is audited | Label changes, refusals and files given to apps go to the DLP audit log (`dlp` in the Terminal). |

## 3. Untrusted apps see only what you choose

Apps get one of three ways to reach files:

- **CAP_FILES** lets an app name any path. Only the Terminal (a system app) and
  Files, the file explorer, have it; reading still taints them like any app.
- **CAP_STORAGE** gives an app a private folder, `/apps/<name>`. The app
  can't name anything outside it, and `..` is never accepted.
- **CAP_PICK** lets an app ask *you*. The kernel shows the file picker, and
  the app gets an open handle to the one file you chose. It never sees the
  folder listing or the file's path.

Notes is an untrusted app built this way. The Craft apps will run the same way.

For untrusted apps, a folder's entries are data too: file names, and the
sizes listed beside them. When such an app holding Confidential data adds,
renames or removes something in a folder, the folder's label rises to match
(and it's audited), and looking inside a folder taints the app with the
folder's label; stat taints with the file's label, and listings leave out the
sizes of files labelled above what the app holds. Otherwise an app could
spell out a secret in file names for a later, untainted run of itself to read
back and send.

Listing a folder is itself a metadata read: `readdir` and the Linux
`getdents64` call taint an untrusted app with the label of every entry they
return, even if the app never opens the file. A folder containing one
Confidential entry therefore makes an untrusted listing app Confidential for
the rest of its run, and it loses network egress. That is deliberate: file
names and even the presence of an entry can carry information.

The system dialogs (the file picker, label changes, the firewall) ignore keys
and clicks for a moment after they appear, so a dialog an app opens can't
catch an Enter you meant for something else; and saving over an existing file
in the picker takes a second, deliberate Save.

## Try it

```
write plan.txt launch on friday
mkdir work
write work/salary.txt alice 95000
label work/salary.txt confidential
cat work/salary.txt        (the Terminal's badge turns Confidential)
write copy.txt hello       (copy.txt is created Confidential)
label copy.txt public      (the kernel asks you)
dlp                        (the audit log)
notes                      (Ctrl+O opens the picker)
```
