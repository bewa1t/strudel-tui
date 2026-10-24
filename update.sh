#!/bin/zsh
# Pull upstream Strudel, rebuild the REPL, refresh the bundled chrome-headless-shell,
# and install everything: data -> ~/Library/Application Support/strudel, binary -> ~/.local/bin/strudel
#   ./update.sh         always rebuild
#   ./update.sh --auto  only when upstream has new commits (used by launchd)
set -euo pipefail
cd "${0:A:h}"

HOME_DIR="${STRUDEL_HOME:-$HOME/Library/Application Support/strudel}"
REPO=https://codeberg.org/uzu/strudel.git

[[ -d upstream ]] || git clone "$REPO" upstream
git -C upstream fetch --quiet origin
if [[ "${1:-}" == --auto && -x "$HOME/.local/bin/strudel" && "$(git -C upstream rev-parse HEAD)" == "$(git -C upstream rev-parse '@{u}')" ]]; then
  echo "$(date) up to date"; exit 0
fi
git -C upstream pull --ff-only --quiet
echo "$(date) building $(git -C upstream log -1 --format='%h %s')"

command -v pnpm >/dev/null || corepack enable
(cd upstream && pnpm i --frozen-lockfile && pnpm build)
mkdir -p "$HOME_DIR"
rsync -a --delete upstream/website/dist/ "$HOME_DIR/dist/"
cp upstream/doc.json "$HOME_DIR/doc.json"
node -e 'import(process.argv[1]).then(m => console.log(JSON.stringify(m.default)))' \
  "$PWD/upstream/packages/soundfonts/gm.mjs" > "$HOME_DIR/gm.json"

# private headless chrome (no Dock icon, separate from any installed Chrome)
out=$(npx -y @puppeteer/browsers@latest install chrome-headless-shell@stable --path "$HOME_DIR/chrome" | tail -1)
bin="${out#* }"
ln -sfn "$bin" "$HOME_DIR/chrome-headless-shell"
# drop older chrome versions
ver_dir="${bin%/chrome-headless-shell-mac*}"
find "$HOME_DIR/chrome/chrome-headless-shell" -mindepth 1 -maxdepth 1 ! -path "$ver_dir" -exec rm -rf {} +

cargo install --quiet --path . --root "$HOME/.local" --force
echo "$(date) installed $HOME/.local/bin/strudel ($(git -C upstream log -1 --format=%h))"
