#ifndef MYME_BRIDGE_H
#define MYME_BRIDGE_H

// Import the myme C API so that Swift can call the Rust FFI functions directly
// through the clang importer.  The header is referenced relative to the
// repository root; the build script passes -I flags to make this resolvable.
#include "myme.h"

#endif /* MYME_BRIDGE_H */
