const docsCopyStatus = document.querySelector('#docs-copy-status');

if (docsCopyStatus && navigator.clipboard) {
  for (const button of document.querySelectorAll<HTMLButtonElement>('[data-copy-code]')) {
    const code = document.getElementById(button.dataset.copyCode ?? '');
    if (!code) continue;
    let reset: number | undefined;
    button.hidden = false;
    button.addEventListener('click', async () => {
      window.clearTimeout(reset);
      try {
        await navigator.clipboard.writeText(code.textContent ?? '');
        docsCopyStatus.textContent = 'code copied.';
        button.textContent = 'copied';
        reset = window.setTimeout(() => {
          button.textContent = 'copy';
        }, 1600);
      } catch {
        getSelection()?.selectAllChildren(code);
        button.textContent = 'copy';
        docsCopyStatus.textContent = 'copy the selected code.';
      }
    });
  }
}
