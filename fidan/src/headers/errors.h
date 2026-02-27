// Copyright (c) Kaan Gönüldinc (AppSolves). All rights reserved.
// See LICENSE file in the project root for full license information.

#ifndef FIDAN_ERRORS_H
#define FIDAN_ERRORS_H

// Include necessary headers
#include <exception>
#include <string>
#include <vector>
#include <sstream>
#include <unordered_map>

// Preprocess the source code into a map for O(1) line retrieval
std::unordered_map<int, std::string> preprocessSource(const std::string_view &source) noexcept;

// This function retrieves the source line from the preprocessed map
std::string getSourceLine(int lineNumber, const std::unordered_map<int, std::string> &sourceMap) noexcept;

// Enum class for the type of the error
enum class FidanErrorType
{
    SyntaxError,
    LogicError,
    RuntimeError,
    ValueError,
    TypeError,
    DeclarationError
};

// Base class for exceptions in Fidan language
class FidanException : public std::exception
{
protected:
    // Message, line, column, and traceback of the exception
    std::string message;
    int line, column;
    std::vector<std::string> traceback;

public:
    // Constructor
    FidanException(const std::string &msg, int line = -1, int column = -1)
        : message(msg), line(line), column(column) {}

    // Method to get the message of the exception
    const char *what() const noexcept override
    {
        return message.c_str();
    }

    // Method to get the line, column and type of the exception
    int getLine() const { return line; }
    int getColumn() const { return column; }
    // Method to get type of the exception
    inline virtual FidanErrorType getType() const = 0;

    // Method to add a traceback entry
    inline void addTrace(const std::string &file, const std::string &function, int line)
    {
        std::stringstream trace;
        trace << "  --> File \"" << file << "\", line " << line << ", in " << function;
        traceback.push_back(trace.str());
    }

    // Method to get the traceback as a string
    std::string getTraceback() const
    {
        std::stringstream traceString;
        traceString << "Traceback (most recent call last):" << std::endl;
        for (const auto &trace : traceback)
        {
            traceString << trace << std::endl;
        }
        return traceString.str();
    }
};

// TraceGuard class to automatically manage traceback
class TraceGuard
{
private:
    // Reference to the exception, file, function, and line
    FidanException &exception;
    const std::string &file;
    const std::string &function;
    int line;

public:
    // Constructor
    TraceGuard(FidanException &ex, const std::string &file, const std::string &function, int line)
        : exception(ex), file(file), function(function), line(line)
    {
        // Automatically add traceback entry on creation
        exception.addTrace(file, function, line);
    }

    // Destructor
    ~TraceGuard()
    {
        // Destructor automatically removes the trace entry
    }
};

// SyntaxError class
class SyntaxError : public FidanException
{
public:
    // Constructor
    SyntaxError(const std::string &msg, int line = -1, int column = -1)
        : FidanException("SyntaxError: " + msg, line, column) {}

    // Method to get the type of the exception
    inline FidanErrorType getType() const override
    {
        return FidanErrorType::SyntaxError;
    }
};

// LogicError class
class LogicError : public FidanException
{
public:
    // Constructor
    LogicError(const std::string &msg, int line = -1, int column = -1)
        : FidanException("LogicError: " + msg, line, column) {}

    // Method to get the type of the exception
    inline FidanErrorType getType() const override
    {
        return FidanErrorType::LogicError;
    }
};

// ValueError class
class ValueError : public FidanException
{
public:
    // Constructor
    ValueError(const std::string &msg, int line = -1, int column = -1)
        : FidanException("ValueError: " + msg, line, column) {}

    // Method to get the type of the exception
    inline FidanErrorType getType() const override
    {
        return FidanErrorType::RuntimeError;
    }
};

// TypeError class
class TypeError : public FidanException
{
public:
    // Constructor
    TypeError(const std::string &msg, int line = -1, int column = -1)
        : FidanException("TypeError: " + msg, line, column) {}

    // Method to get the type of the exception
    inline FidanErrorType getType() const override
    {
        return FidanErrorType::TypeError;
    }
};

// DeclarationError class
class DeclarationError : public FidanException
{
public:
    // Constructor
    DeclarationError(const std::string &msg, int line = -1, int column = -1)
        : FidanException("DeclarationError: " + msg, line, column) {}

    // Method to get the type of the exception
    inline FidanErrorType getType() const override
    {
        return FidanErrorType::DeclarationError;
    }
};

// RuntimeError class
class RuntimeError : public FidanException
{
public:
    // Constructor
    RuntimeError(const std::string &msg, int line = -1, int column = -1)
        : FidanException("RuntimeError: " + msg, line, column) {}

    // Method to get the type of the exception
    inline FidanErrorType getType() const override
    {
        return FidanErrorType::RuntimeError;
    }
};

#endif // FIDAN_ERRORS_H