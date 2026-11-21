const command = document.querySelector('#install-command');
const copy = document.querySelector<HTMLButtonElement>('#copy-install');
const copyStatus = document.querySelector('#copy-status');

if (command && copy && copyStatus && navigator.clipboard) {
  let reset: number | undefined;
  copy.hidden = false;
  copy.addEventListener('click', async () => {
    window.clearTimeout(reset);
    try {
      await navigator.clipboard.writeText(command.textContent ?? '');
      copy.dataset.copied = 'true';
      copyStatus.textContent = 'install command copied.';
    } catch {
      getSelection()?.selectAllChildren(command);
      delete copy.dataset.copied;
      copyStatus.textContent = 'copy the selected install command.';
    }
    reset = window.setTimeout(() => {
      delete copy.dataset.copied;
      copyStatus.textContent = '';
    }, 1800);
  });
}

async function showStars() {
  const github = document.querySelector('#github-link');
  const stars = document.querySelector<HTMLElement>('#github-stars');
  const value = document.querySelector('#github-star-value');
  if (!github || !stars || !value) return;

  try {
    const response = await fetch('https://api.github.com/repos/fatihguzeldev/ring64', {
      signal: AbortSignal.timeout(4000),
    });
    if (!response.ok) return;
    const repository: unknown = await response.json();
    if (
      !repository ||
      typeof repository !== 'object' ||
      !('stargazers_count' in repository) ||
      typeof repository.stargazers_count !== 'number' ||
      !Number.isSafeInteger(repository.stargazers_count) ||
      repository.stargazers_count < 0
    )
      return;
    const count = new Intl.NumberFormat().format(repository.stargazers_count);
    value.textContent = count;
    stars.hidden = false;
    github.setAttribute('aria-label', `github, ${count} ${count === '1' ? 'star' : 'stars'}`);
  } catch {
    // The link stays usable when the count cannot be fetched.
  }
}

const canvas = document.querySelector<HTMLCanvasElement>('#ink');
if (canvas && !canvas.dataset.frame) {
  // Keep the metadata request out of the first frame's loading path.
  const observer = new MutationObserver(() => loadStars());
  const timeout = window.setTimeout(loadStars, 3000);
  observer.observe(canvas, { attributes: true, attributeFilter: ['data-frame'] });

  function loadStars() {
    observer.disconnect();
    window.clearTimeout(timeout);
    void showStars();
  }
} else {
  void showStars();
}
