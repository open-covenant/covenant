/**
 * Keep one Node notice out of the command output.
 *
 * The database is Node's own SQLite, which announces itself as experimental the
 * first time it is loaded. Node prints that from a default listener, so the
 * listener is replaced rather than added to, and this module is imported before
 * anything that touches the database. Every other warning still prints.
 */

process.removeAllListeners('warning');
process.on('warning', (warning) => {
  if (warning.name === 'ExperimentalWarning' && warning.message.includes('SQLite')) return;
  process.stderr.write(`${warning.stack ?? warning.message}\n`);
});
