// Authenticode signing for releases, so Windows SmartScreen and antivirus trust the installer and the engine.
// Signing is optional: set AFTERCLIP_SIGN_COMMAND to a command that signs the file in %1, for example
//   signtool sign /fd sha256 /tr http://timestamp.digicert.com /td sha256 /sha1 <certificate thumbprint> "%1"
// or the command your signing service gives you, such as Azure Trusted Signing or SignPath. Without it, releases
// are built unsigned, as before. The updater's own signature is separate and always required.
const { execSync } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');

const command = process.env.AFTERCLIP_SIGN_COMMAND?.trim() || null;

/** The command, with %1 where the file goes, or null when signing is off. */
function signCommand() {
  if (command && !command.includes('%1')) throw new Error('AFTERCLIP_SIGN_COMMAND must contain %1 where the file to sign goes.');
  return command;
}

/** Each file's Authenticode status, such as Valid or NotSigned. */
function signatureStatus(files) {
  if (!files.length) return {};
  const list = files.map((f) => `'${path.resolve(f).replace(/'/g, "''")}'`).join(',');
  const out = execSync(`powershell -NoProfile -Command "Get-AuthenticodeSignature -LiteralPath ${list} | ForEach-Object { $_.Path + '|' + $_.Status }"`, { encoding: 'utf8' });
  const status = {};
  for (const line of out.split(/\r?\n/).filter(Boolean)) {
    const [file, state] = line.split('|');
    status[path.resolve(file).toLowerCase()] = state.trim();
  }
  return status;
}

/** Signs any of `files` that aren't signed yet, then checks each one is. */
function signFiles(files) {
  const sign = signCommand();
  if (!sign) return;
  const existing = files.filter((f) => fs.existsSync(f));
  const before = signatureStatus(existing);
  for (const file of existing) {
    if (before[path.resolve(file).toLowerCase()] === 'Valid') continue;
    console.log(`Signing ${path.basename(file)}`);
    execSync(sign.replace(/%1/g, path.resolve(file)), { stdio: 'inherit' });
  }
  verifySigned(existing);
}

/** Throws unless every file has a valid signature. */
function verifySigned(files) {
  const status = signatureStatus(files);
  const bad = files.filter((f) => status[path.resolve(f).toLowerCase()] !== 'Valid');
  if (bad.length) throw new Error(`Not signed: ${bad.map((f) => `${path.basename(f)} (${status[path.resolve(f).toLowerCase()] ?? 'unknown'})`).join(', ')}`);
}

module.exports = { signCommand, signFiles, verifySigned };
