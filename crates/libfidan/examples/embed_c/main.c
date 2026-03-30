#include "fidan.h"

#include <stdint.h>
#include <stdio.h>
#include <string.h>

int main(void)
{
  static const char *source = "var result = \"Hello from libfidan\"\n";

  FidanVm *vm = fidan_vm_new();
  FidanValue *value = fidan_eval(vm, (const uint8_t *)source, strlen(source));

  if (value == NULL)
  {
    const char *err = fidan_vm_last_error(vm);
    fprintf(stderr, "libfidan error: %s\n", err ? err : "(unknown)");
    fidan_vm_free(vm);
    return 1;
  }

  FidanValue *text = fidan_value_to_string(value);
  const uint8_t *bytes = fidan_value_string_bytes(text);
  size_t len = fidan_value_string_len(text);
  fwrite(bytes, 1, len, stdout);
  fputc('\n', stdout);

  fidan_value_free(text);
  fidan_value_free(value);
  fidan_vm_free(vm);
  return 0;
}
