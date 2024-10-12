// Copyright (c) AppSolves (Kaan Gönüldinc). All rights reserved.
// See LICENSE file in the project root for full license information.

#ifndef FIDAN_HELPERS_H
#define FIDAN_HELPERS_H

#include "parser.h"
#include <string>
#include <algorithm>
#include <variant>
#include <unordered_map>
#include <functional>
#include <vector>
#include <stdexcept>
#include <memory>
#include <cmath>
#include <stack>
#include <optional>

std::string upper(std::string_view input);
std::string lower(std::string_view input);

struct BracketError
{
    Token token;    // The unmatched token
    bool isOpening; // True if the token is an opening bracket, false if it's a closing bracket
};

std::optional<BracketError> checkBalancedBrackets(const std::vector<Token> &tokens);

#endif // FIDAN_HELPERS_H