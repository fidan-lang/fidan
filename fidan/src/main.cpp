// Copyright (c) AppSolves (Kaan Gönüldinc). All rights reserved.
// See LICENSE file in the project root for full license information.

#include "headers/tokenizer.h"
#include "headers/parser.h"
#include "headers/interpreter.h"
#include "headers/errors.h"
#include "headers/ast.h"
#include "headers/main.h"
#include "headers/helpers.h"

#include <iostream>
#include <fstream>
#include <filesystem>
#include <functional>
// https://github.com/p-ranav/argparse
#include <argparse/argparse.hpp>

int runREPL()
{
    std::cout << "Fidan " << version << " REPL" << std::endl;
    std::cout << "COMMANDS:" << std::endl;
    std::cout << "--> Type 'help', 'license', or 'credits' for more information" << std::endl;
    std::cout << "--> Type 'exit' to exit or 'clear' to clear the screen" << std::endl;

    std::string line;

    while (true)
    {
        std::cout << ">>> ";
        std::getline(std::cin, line);

        if (line == "exit")
        {
            break;
        }

        auto command = commands.find(line);
        if (command != commands.end())
        {
            command->second();
        }
        else
        {
            // Process the line as source code
            // (Assuming you have a function to handle this)
            // processSourceLine(line);
        }
    }

    return 0;
}

int main(int argc, char *argv[])
{
#ifdef DEBUG_MODE
    std::cout << ">>>DEBUG MODE IS ACTIVE<<<" << std::endl
              << std::endl;
#endif

    argparse::ArgumentParser program("fidan", version);

    program.add_argument("source_file")
        .help("defines the source file to run")
        .metavar("SOURCE_FILE")
        .default_value(std::string("REPL"))
        .nargs(1);

    try
    {
        program.parse_args(argc, argv);
    }
    catch (const std::exception &err)
    {
        std::cerr << err.what() << std::endl;
        std::cerr << program;
        return 1;
    }

    if (program.is_used("source_file") == false || upper(program.get<std::string>("source_file")) == "REPL")
    {
        return runREPL();
    }

    std::string filename = program.get<std::string>("source_file");
    if ((filename.rfind(".fdn") != filename.size() - 4) && (filename.rfind(".fidan") != filename.size() - 6))
    {
        std::cerr << "FidanFileError: File must have '.fdn' or '.fidan' extension" << std::endl;
        return 1;
    }

    std::ifstream file(filename, std::ios::binary);
    if (!file || !file.good())
    {
        std::cerr << "FidanFileError: File '" << filename << "' not found" << std::endl;
        return 1;
    }
    if (!file.is_open())
    {
        std::cerr << "FidanFileError: Failed to open file '" << filename << "'" << std::endl;
        return 1;
    }
    // Read the file contents into a string
    std::string source((std::istreambuf_iterator<char>(file)),
                       std::istreambuf_iterator<char>());

    try
    {
        std::string full_file_path = std::filesystem::absolute(filename).string();
        Tokenizer tokenizer(source, full_file_path);
        std::vector<Token> tokens = tokenizer.tokenize();

        Parser parser(tokens, full_file_path);
        std::vector<std::unique_ptr<ASTNode>> ast = parser.parse();

        for (const auto &node : ast)
        {
            std::cout << node->toString() << std::endl;
        }

        InterpreterVM interpreter(ast);
        interpreter.interpret();

        return 0;
    }
    catch (const FidanException &e)
    {
        std::unordered_map<int, std::string> sourceMap = preprocessSource(source);
        std::cerr << e.getTraceback() << std::endl;
        std::cerr << "  " << getSourceLine(e.getLine(), sourceMap) << std::endl;
        std::cerr << "  " << std::string(e.getColumn() - 2, ' ') << "^" << std::endl;
        std::cerr << std::endl;
        std::cerr << e.what() << " at line " << e.getLine() << ", column " << e.getColumn() << std::endl;
        return 1;
    }
    catch (const std::exception &e)
    {
        std::cerr << "RuntimeError: " << e.what() << std::endl;
        return 1;
    }
    catch (...)
    {
        std::cerr << "UnknownError: An unknown error occurred" << std::endl;
        return 1;
    }
}