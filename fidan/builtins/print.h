// Copyright (c) AppSolves (Kaan Gönüldinc). All rights reserved.
// See LICENSE file in the project root for full license information.

#ifndef FIDAN_BUILTINS_PRINT_H
#define FIDAN_BUILTINS_PRINT_H

#include <string>
#include <iostream>

// Print a message to the console
void print(const std::string &message)
{
    std::cout << message << std::endl;
}

#endif // BUILTINS_H