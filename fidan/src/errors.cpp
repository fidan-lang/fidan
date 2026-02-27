// Copyright (c) Kaan Gönüldinc (AppSolves). All rights reserved.
// See LICENSE file in the project root for full license information.

#include "headers/errors.h"

// Preprocess the source code into a map for O(1) line retrieval
std::unordered_map<int, std::string> preprocessSource(const std::string_view &source) noexcept
{
    std::unordered_map<int, std::string> sourceMap;
    int currentLine = 1;
    std::string line;
    std::stringstream sourceStream(source.data());

    while (std::getline(sourceStream, line))
    {
        sourceMap[currentLine] = line;
        currentLine++;
    }
    return sourceMap;
}

// This function retrieves the source line from the preprocessed map
std::string getSourceLine(int lineNumber, const std::unordered_map<int, std::string> &sourceMap) noexcept
{
    auto it = sourceMap.find(lineNumber);
    if (it != sourceMap.end())
    {
        return it->second;
    }
    return "";
}