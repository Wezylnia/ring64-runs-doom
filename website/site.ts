function enableCopyButtons() {
  const status = document.getElementById('copy-status');
  if (!status || !navigator.clipboard) return;

  let latestRequest = 0;
  for (const button of document.querySelectorAll<HTMLButtonElement>('[data-copy-code]')) {
    const source = document.getElementById(button.dataset.copyCode ?? '');
    if (!source) continue;
    const install = button.id === 'copy-install';
    const description = install ? 'install command' : 'code';
    let reset: number | undefined;
    const showCopied = (copied: boolean) => {
      if (copied) button.dataset.copied = 'true';
      else delete button.dataset.copied;
      if (!install) button.textContent = copied ? 'copied' : 'copy';
    };
    button.hidden = false;
    button.addEventListener('click', async () => {
      const request = ++latestRequest;
      window.clearTimeout(reset);
      showCopied(false);
      try {
        await navigator.clipboard.writeText(source.textContent ?? '');
        if (request !== latestRequest) return;
        showCopied(true);
        status.textContent = `${description} copied.`;
      } catch {
        if (request !== latestRequest) return;
        getSelection()?.selectAllChildren(source);
        status.textContent = `copy the selected ${description}.`;
        if (!install) return;
      }
      reset = window.setTimeout(
        () => {
          showCopied(false);
          if (request === latestRequest) status.textContent = '';
        },
        install ? 1800 : 1600,
      );
    });
  }
}

enableCopyButtons();
