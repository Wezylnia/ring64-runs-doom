const command = document.querySelector('#install-command');
const button = document.querySelector<HTMLButtonElement>('#copy-install');
const copyStatus = document.querySelector('#copy-status');
let reset: number | undefined;

if (command && button && copyStatus && navigator.clipboard) {
  button.hidden = false;
  button.addEventListener('click', async () => {
    clearTimeout(reset);
    try {
      await navigator.clipboard.writeText(command.textContent ?? '');
      button.textContent = 'Copied';
      copyStatus.textContent = 'Install command copied.';
    } catch {
      getSelection()?.selectAllChildren(command);
      copyStatus.textContent = 'Copy the selected install command.';
    }
    reset = setTimeout(() => {
      button.textContent = 'Copy';
      copyStatus.textContent = '';
    }, 1800);
  });
}
