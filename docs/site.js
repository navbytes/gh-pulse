document.querySelectorAll('[data-copy]').forEach((button) => {
  button.addEventListener('click', async () => {
    const command = document.getElementById(button.dataset.copy);
    if (!command) return;
    const status = button.closest('.install-panel')?.querySelector('.install-copy-status') || document.getElementById('copy-status');

    try {
      await navigator.clipboard.writeText(command.textContent.trim());
      button.textContent = 'Copied';
      status.textContent = 'Command copied to clipboard.';
      window.setTimeout(() => { button.textContent = 'Copy'; }, 2200);
    } catch {
      const selection = window.getSelection();
      const range = document.createRange();
      range.selectNodeContents(command);
      selection.removeAllRanges();
      selection.addRange(range);
      button.textContent = 'Selected';
      status.textContent = window.matchMedia('(pointer: coarse)').matches
        ? 'Command selected. Use your browser’s Copy action.'
        : 'Command selected. Press Cmd+C or Ctrl+C.';
      window.setTimeout(() => { button.textContent = 'Copy'; }, 2200);
    }
  });
});
