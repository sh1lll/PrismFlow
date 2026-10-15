// PrismFlow preload for the gateway child (MIT, PrismFlow's own code).
// The broker holds this process's stdin open; when PrismFlow exits for any
// reason, including a crash, stdin reaches EOF and the gateway exits too.
// That prevents an orphaned gateway from holding upstream keys in its env.
'use strict';
process.stdin.on('end', () => process.exit(0));
process.stdin.on('error', () => process.exit(0));
process.stdin.resume();
