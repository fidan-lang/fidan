#ifndef FIDAN_H
#define FIDAN_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C"
{
#endif

  typedef struct FidanVm FidanVm;
  typedef struct FidanValue FidanValue;

  /*
   * Create / destroy a Fidan VM.
   *
   * The VM owns the last-error buffer returned by `fidan_vm_last_error()`.
   * Returned pointers remain valid until the next VM operation that updates
   * the error state or until `fidan_vm_free()`.
   */
  FidanVm *fidan_vm_new(void);
  void fidan_vm_free(FidanVm *vm);

  /*
   * Override the base directory used to resolve relative imports for in-memory
   * evaluation. Returns 1 on success, 0 on failure.
   */
  int8_t fidan_vm_set_base_dir(FidanVm *vm, const char *path);

  /*
   * Return the last VM error as a borrowed NUL-terminated UTF-8 string.
   * Returns NULL when no error is recorded.
   */
  const char *fidan_vm_last_error(const FidanVm *vm);

  /*
   * Evaluate Fidan source from memory or a file path.
   *
   * Contract:
   * - On success, returns an owned `FidanValue*`.
   * - On failure, returns NULL and records the message in `fidan_vm_last_error()`.
   * - The current initial embedding slice returns the top-level `result` binding
   *   when present. Otherwise a successful run returns `nothing`.
   */
  FidanValue *fidan_eval(FidanVm *vm, const uint8_t *source, size_t len);
  FidanValue *fidan_eval_file(FidanVm *vm, const char *path);

  /* Value ownership helpers. */
  void fidan_value_free(FidanValue *value);
  FidanValue *fidan_value_clone(FidanValue *value);

  /* Basic reflection / conversion helpers. Returned values are owned. */
  FidanValue *fidan_value_type_name(FidanValue *value);
  FidanValue *fidan_value_to_string(FidanValue *value);

  /* Scalar extraction helpers. Non-matching values coerce to 0 / false. */
  int64_t fidan_value_as_int(FidanValue *value);
  double fidan_value_as_float(FidanValue *value);
  int8_t fidan_value_as_bool(FidanValue *value);
  int8_t fidan_value_is_nothing(FidanValue *value);

  /*
   * Borrowed UTF-8 access for string values.
   *
   * The returned pointer is borrowed from `value` and remains valid only while
   * that value stays alive.
   */
  size_t fidan_value_string_len(FidanValue *value);
  const uint8_t *fidan_value_string_bytes(FidanValue *value);

#ifdef __cplusplus
}
#endif

#endif
