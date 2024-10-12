// Copyright (c) AppSolves (Kaan Gönüldinc). All rights reserved.
// See LICENSE file in the project root for full license information.

#ifndef FIDAN_AST_H
#define FIDAN_AST_H

#include <string>
#include <string_view>
#include <vector>
#include <memory>
#include <numeric>
#include <unordered_map>
#include <sstream>
#include "errors.h"
#include "tokenizer.h"

enum class NodeType
{
    VariableDeclaration,
    VariableAssignment,
    ActionDeclaration,
    ObjectDeclaration,
    Block,
    Literal,
    ListLiteral,
    DynamicLiteral,
    BinaryExpression,
    UnaryExpression,
    VariableReference,
    FunctionCall,
    ReturnStatement,
    Decorator,
    NonNullExpression,
    WhenStatement,
    TryCatchStatement,
    ThrowStatement,
    ForLoop,
    WhileLoop
};

class ASTNode
{
public:
    virtual ~ASTNode() = default;
    virtual std::string toString() const = 0;
    virtual NodeType getType() const = 0;
};

struct Parameter
{
    std::string_view name;
    bool isOptional;
    std::string_view type;
    std::unique_ptr<ASTNode> defaultValue;
};

enum class ScopeType
{
    Global,
    Local,
    Nonlocal,
    ObjectPaired,
    DecoratorPaired
};

struct Scope
{
    std::string_view name;
    ScopeType type;
    std::vector<std::unique_ptr<ASTNode>> statements;
    std::vector<std::shared_ptr<Scope>> children;
    std::weak_ptr<Scope> parent;

    Scope(std::string_view name, ScopeType type, std::shared_ptr<Scope> parent = nullptr)
        : name(name), type(type), parent(parent) {}
};

class ScopeManager
{
private:
    std::vector<std::shared_ptr<Scope>> scopes;

public:
    void enterScope(std::string_view name, ScopeType type)
    {
        std::shared_ptr<Scope> parent = scopes.empty() ? nullptr : scopes.back();
        scopes.push_back(std::make_shared<Scope>(name, type, parent));
    }

    void exitScope()
    {
        if (!scopes.empty())
        {
            scopes.pop_back();
        }
    }

    std::shared_ptr<Scope> currentScope()
    {
        return scopes.empty() ? nullptr : scopes.back();
    }
};

class VariableDeclaration : public ASTNode
{
public:
    std::vector<std::string_view> identifierParts;
    std::string_view type;
    std::unique_ptr<ASTNode> initializer;
    std::shared_ptr<Scope> scope;

    VariableDeclaration(const std::vector<std::string_view> &identifierParts, std::string_view type, std::unique_ptr<ASTNode> initializer, std::shared_ptr<Scope> scope)
        : identifierParts(identifierParts), type(type), initializer(std::move(initializer)), scope(scope) {}

    std::string toString() const override
    {
        std::ostringstream oss;
        oss << "VariableDeclaration(name = " << std::accumulate(identifierParts.begin(), identifierParts.end(), std::string{}, [](const std::string &a, std::string_view b)
                                                                { return a + std::string(b); })
            << ", type = " << type
            << ", scope = " << (scope ? scope->name : "null")
            << ", scopeType = " << static_cast<int>(scope->type)
            << ", initializer = " << (initializer ? initializer->toString() : "null") << ")";
        return oss.str();
    }

    NodeType getType() const override
    {
        return NodeType::VariableDeclaration;
    }
};

class VariableAssignment : public ASTNode
{
public:
    std::vector<std::string_view> identifierParts;
    std::unique_ptr<ASTNode> value;
    std::shared_ptr<Scope> scope;

    VariableAssignment(const std::vector<std::string_view> &identifierParts, std::unique_ptr<ASTNode> value, std::shared_ptr<Scope> scope)
        : identifierParts(identifierParts), value(std::move(value)), scope(scope) {}

    std::string toString() const override
    {
        std::ostringstream oss;
        oss << "VariableAssignment(name = " << std::accumulate(identifierParts.begin(), identifierParts.end(), std::string{}, [](const std::string &a, std::string_view b)
                                                               { return a + std::string(b); })
            << ", scope = " << (scope ? scope->name : "null")
            << ", scopeType = " << static_cast<int>(scope->type)
            << ", value = " << (value ? value->toString() : "null") << ")";
        return oss.str();
    }

    NodeType getType() const override
    {
        return NodeType::VariableAssignment;
    }
};

class Block : public ASTNode
{
public:
    std::vector<std::unique_ptr<ASTNode>> statements;
    std::shared_ptr<Scope> scope;

    Block(std::shared_ptr<Scope> scope = nullptr) : scope(scope) {}

    std::string toString() const override
    {
        std::ostringstream oss;
        oss << "Block(" << statements.size() << " statements, scope = " << (scope ? scope->name : "null") << ")";
        return oss.str();
    }

    NodeType getType() const override
    {
        return NodeType::Block;
    }
};

class ActionDeclaration : public ASTNode
{
public:
    std::string_view name;
    std::string_view parentName;
    std::vector<Parameter> parameters;
    std::string_view returnType;
    std::unique_ptr<ASTNode> body;
    std::shared_ptr<Scope> scope;

    ActionDeclaration(std::string_view name, std::string_view parentName, std::vector<Parameter> parameters, std::string_view returnType, std::unique_ptr<ASTNode> body, std::shared_ptr<Scope> scope)
        : name(name), parentName(parentName), parameters(std::move(parameters)), returnType(returnType), body(std::move(body)), scope(scope)
    {
        if (body && body->getType() != NodeType::Block)
        {
            throw RuntimeError("'Action/Function' body must be of type <Block>", -1, -1);
        }
    }

    std::string toString() const override
    {
        std::ostringstream oss;
        oss << "ActionDeclaration(" << name << ", parent = " << (parentName.empty() ? "None" : parentName) << ", parameters = [";

        for (size_t i = 0; i < parameters.size(); ++i)
        {
            const auto &param = parameters[i];
            oss << (param.isOptional ? "optional " : "") << param.name << " oftype " << (param.type.empty() ? "any" : param.type);

            if (param.defaultValue)
            {
                oss << " default " << param.defaultValue->toString();
            }

            if (i < parameters.size() - 1)
            {
                oss << ", ";
            }
        }

        oss << "], body = " << (body ? body->toString() : "null")
            << ", returnType = " << returnType
            << ", scope = " << (scope ? scope->name : "null") << ")";
        return oss.str();
    }

    NodeType getType() const override
    {
        return NodeType::ActionDeclaration;
    }
};

class ObjectDeclaration : public ASTNode
{
public:
    std::string_view name;
    std::string_view parentName;
    std::vector<std::unique_ptr<ASTNode>> statements;
    std::shared_ptr<Scope> scope;

    ObjectDeclaration(std::string_view name, std::string_view parentName, std::shared_ptr<Scope> scope = nullptr)
        : name(name), parentName(parentName), scope(scope) {}

    const std::vector<std::unique_ptr<ASTNode>> &getStatements() const
    {
        return statements;
    }

    std::string toString() const override
    {
        std::ostringstream oss;
        oss << "ObjectDeclaration(" << name << ", parent = " << (parentName.empty() ? "None" : parentName)
            << ", scope = " << (scope ? scope->name : "null")
            << ", " << statements.size() << " statements)";
        return oss.str();
    }

    NodeType getType() const override
    {
        return NodeType::ObjectDeclaration;
    }
};

class Literal : public ASTNode
{
public:
    Token value;
    std::shared_ptr<Scope> scope;

    explicit Literal(Token value, std::shared_ptr<Scope> scope = nullptr) : value(value), scope(scope) {}

    std::string toString() const override
    {
        std::ostringstream oss;
        oss << "Literal(" << value.value << ", scope = " << (scope ? scope->name : "null") << ")";
        return oss.str();
    }

    NodeType getType() const override
    {
        return NodeType::Literal;
    }
};

class ListLiteral : public ASTNode
{
public:
    std::vector<std::unique_ptr<ASTNode>> elements;
    std::shared_ptr<Scope> scope;

    explicit ListLiteral(std::vector<std::unique_ptr<ASTNode>> elements, std::shared_ptr<Scope> scope = nullptr)
        : elements(std::move(elements)), scope(scope) {}

    std::string toString() const override
    {
        std::ostringstream oss;
        oss << "ListLiteral(" << elements.size() << " elements, scope = " << (scope ? scope->name : "null") << ")";
        return oss.str();
    }

    NodeType getType() const override
    {
        return NodeType::ListLiteral;
    }
};

class DynamicLiteral : public ASTNode
{
public:
    std::shared_ptr<Scope> scope;

    DynamicLiteral(std::shared_ptr<Scope> scope = nullptr) : scope(scope) {}

    std::string toString() const override
    {
        std::ostringstream oss;
        oss << "DynamicLiteral(scope = " << (scope ? scope->name : "null") << ")";
        return oss.str();
    }

    NodeType getType() const override
    {
        return NodeType::DynamicLiteral;
    }
};

class BinaryExpression : public ASTNode
{
public:
    std::unique_ptr<ASTNode> left;
    std::unique_ptr<ASTNode> right;
    Token op;
    std::shared_ptr<Scope> scope;

    BinaryExpression(std::unique_ptr<ASTNode> left, Token op, std::unique_ptr<ASTNode> right, std::shared_ptr<Scope> scope = nullptr)
        : left(std::move(left)), right(std::move(right)), op(op), scope(scope) {}

    std::string toString() const override
    {
        std::ostringstream oss;
        oss << "BinaryExpression(" << left->toString() << " " << op.value << " " << right->toString()
            << ", scope = " << (scope ? scope->name : "null") << ")";
        return oss.str();
    }

    NodeType getType() const override
    {
        return NodeType::BinaryExpression;
    }
};

class UnaryExpression : public ASTNode
{
public:
    std::unique_ptr<ASTNode> operand;
    Token op;
    std::shared_ptr<Scope> scope;

    UnaryExpression(Token op, std::unique_ptr<ASTNode> operand, std::shared_ptr<Scope> scope = nullptr)
        : operand(std::move(operand)), op(op), scope(scope) {}

    std::string toString() const override
    {
        std::ostringstream oss;
        oss << "UnaryExpression(" << op.value << operand->toString()
            << ", scope = " << (scope ? scope->name : "null") << ")";
        return oss.str();
    }

    NodeType getType() const override
    {
        return NodeType::UnaryExpression;
    }
};

class VariableReference : public ASTNode
{
public:
    std::vector<std::string_view> identifierParts;
    std::shared_ptr<Scope> scope;

    explicit VariableReference(const std::vector<std::string_view> &identifierParts, std::shared_ptr<Scope> scope = nullptr)
        : identifierParts(identifierParts), scope(scope) {}

    std::string toString() const override
    {
        std::ostringstream oss;
        oss << "VariableReference(" << std::accumulate(identifierParts.begin(), identifierParts.end(), std::string{}, [](const std::string &a, std::string_view b)
                                                       { return a + std::string(b); })
            << ", scope = " << (scope ? scope->name : "null") << ")";
        return oss.str();
    }

    NodeType getType() const override
    {
        return NodeType::VariableReference;
    }
};

class FunctionCall : public ASTNode
{
public:
    std::vector<std::string_view> identifierParts;
    std::vector<std::unique_ptr<ASTNode>> arguments;
    std::unordered_map<std::string_view, std::unique_ptr<ASTNode>> keywordArguments;
    std::shared_ptr<Scope> scope;

    FunctionCall(std::vector<std::string_view> identifierParts, std::vector<std::unique_ptr<ASTNode>> arguments, std::unordered_map<std::string_view, std::unique_ptr<ASTNode>> keywordArguments, std::shared_ptr<Scope> scope = nullptr)
        : identifierParts(std::move(identifierParts)), arguments(std::move(arguments)), keywordArguments(std::move(keywordArguments)), scope(scope) {}

    std::string toString() const override
    {
        std::ostringstream oss;
        oss << "FunctionCall(" << std::accumulate(identifierParts.begin(), identifierParts.end(), std::string{}, [](const std::string &a, std::string_view b)
                                                  { return a + std::string(b); })
            << ", arguments = [";

        for (size_t i = 0; i < arguments.size(); ++i)
        {
            oss << arguments[i]->toString();

            if (i < arguments.size() - 1)
            {
                oss << ", ";
            }
        }

        oss << "], keyword_arguments = [";

        for (auto it = keywordArguments.begin(); it != keywordArguments.end(); ++it)
        {
            oss << it->first << " = " << it->second->toString();

            if (std::next(it) != keywordArguments.end())
            {
                oss << ", ";
            }
        }

        oss << "], scope = " << (scope ? scope->name : "null") << ")";
        return oss.str();
    }

    NodeType getType() const override
    {
        return NodeType::FunctionCall;
    }
};

class ReturnStatement : public ASTNode
{
public:
    std::unique_ptr<ASTNode> value;
    std::shared_ptr<Scope> scope;

    explicit ReturnStatement(std::unique_ptr<ASTNode> value, std::shared_ptr<Scope> scope = nullptr)
        : value(std::move(value)), scope(scope) {}

    std::string toString() const override
    {
        std::ostringstream oss;
        oss << "ReturnStatement(" << (value ? value->toString() : "null")
            << ", scope = " << (scope ? scope->name : "null") << ")";
        return oss.str();
    }

    NodeType getType() const override
    {
        return NodeType::ReturnStatement;
    }
};

class Decorator : public ASTNode
{
public:
    std::string_view name;
    std::unique_ptr<ASTNode> statement;
    std::shared_ptr<Scope> scope;

    Decorator(std::string_view name, std::unique_ptr<ASTNode> statement, std::shared_ptr<Scope> scope = nullptr)
        : name(name), statement(std::move(statement)), scope(scope) {}

    std::string toString() const override
    {
        std::ostringstream oss;
        oss << "Decorator(" << name << ", statement = " << (statement ? statement->toString() : "null")
            << ", scope = " << (scope ? scope->name : "null") << ")";
        return oss.str();
    }

    NodeType getType() const override
    {
        return NodeType::Decorator;
    }
};

class NonNullExpression : public ASTNode
{
public:
    std::unique_ptr<ASTNode> left;
    std::unique_ptr<ASTNode> right;
    std::shared_ptr<Scope> scope;

    NonNullExpression(std::unique_ptr<ASTNode> left, std::unique_ptr<ASTNode> right, std::shared_ptr<Scope> scope = nullptr)
        : left(std::move(left)), right(std::move(right)), scope(scope) {}

    std::string toString() const override
    {
        std::ostringstream oss;
        oss << "NonNullExpression(" << left->toString() << " ?? " << right->toString()
            << ", scope = " << (scope ? scope->name : "null") << ")";
        return oss.str();
    }

    NodeType getType() const override
    {
        return NodeType::NonNullExpression;
    }
};

class WhenStatement : public ASTNode
{
public:
    std::vector<std::pair<std::unique_ptr<ASTNode>, std::unique_ptr<ASTNode>>> conditionsAndBlocks;
    std::shared_ptr<Scope> scope;

    WhenStatement(std::vector<std::pair<std::unique_ptr<ASTNode>, std::unique_ptr<ASTNode>>> &&conditionsAndBlocks, std::shared_ptr<Scope> scope = nullptr)
        : conditionsAndBlocks(std::move(conditionsAndBlocks)), scope(scope)
    {
        this->conditionsAndBlocks.reserve(this->conditionsAndBlocks.size());

        for (const auto &conditionAndBlock : this->conditionsAndBlocks)
        {
            if (conditionAndBlock.second && conditionAndBlock.second->getType() != NodeType::Block)
            {
                throw RuntimeError("'If/When' body must be of type <Block>", -1, -1);
            }
        }
    }

    std::string toString() const override
    {
        std::ostringstream oss;
        oss << "WhenStatement(" << conditionsAndBlocks.size() << " conditions and blocks"
            << ", scope = " << (scope ? scope->name : "null") << ")";
        return oss.str();
    }

    NodeType getType() const override
    {
        return NodeType::WhenStatement;
    }
};

class TryCatchStatement : public ASTNode
{
public:
    std::unique_ptr<ASTNode> body;
    std::string_view catchIdentifier;
    std::unique_ptr<ASTNode> catchBody;
    std::unique_ptr<ASTNode> finallyBlock;
    std::unique_ptr<ASTNode> elseBlock;
    std::shared_ptr<Scope> scope;

    TryCatchStatement(std::unique_ptr<ASTNode> body, std::string_view catchIdentifier, std::unique_ptr<ASTNode> catchBody, std::unique_ptr<ASTNode> finallyBlock, std::unique_ptr<ASTNode> elseBlock, std::shared_ptr<Scope> scope = nullptr)
        : body(std::move(body)), catchIdentifier(catchIdentifier), catchBody(std::move(catchBody)), finallyBlock(std::move(finallyBlock)), elseBlock(std::move(elseBlock)), scope(scope)
    {
        if (body && body->getType() != NodeType::Block)
        {
            throw RuntimeError("'Try/Attempt' body must be of type <Block>", -1, -1);
        }
        if (catchBody && catchBody->getType() != NodeType::Block)
        {
            throw RuntimeError("'Catch/Except' body must be of type <Block>", -1, -1);
        }
        if (finallyBlock && finallyBlock->getType() != NodeType::Block)
        {
            throw RuntimeError("'Finally/Anyway' body must be of type <Block>", -1, -1);
        }
        if (elseBlock && elseBlock->getType() != NodeType::Block)
        {
            throw RuntimeError("'Else/Otherwise' body must be of type <Block>", -1, -1);
        }
    }

    std::string toString() const override
    {
        std::ostringstream oss;
        oss << "TryCatchStatement(body = " << body->toString()
            << ", catchIdentifier = " << catchIdentifier
            << ", catchBody = " << (catchBody ? catchBody->toString() : "null")
            << ", finallyBlock = " << (finallyBlock ? finallyBlock->toString() : "null")
            << ", elseBlock = " << (elseBlock ? elseBlock->toString() : "null")
            << ", scope = " << (scope ? scope->name : "null") << ")";
        return oss.str();
    }

    NodeType getType() const override
    {
        return NodeType::TryCatchStatement;
    }
};

class ThrowStatement : public ASTNode
{
public:
    std::unique_ptr<ASTNode> value;
    std::shared_ptr<Scope> scope;

    explicit ThrowStatement(std::unique_ptr<ASTNode> value, std::shared_ptr<Scope> scope = nullptr)
        : value(std::move(value)), scope(scope) {}

    std::string toString() const override
    {
        std::ostringstream oss;
        oss << "ThrowStatement(" << (value ? value->toString() : "null")
            << ", scope = " << (scope ? scope->name : "null") << ")";
        return oss.str();
    }

    NodeType getType() const override
    {
        return NodeType::ThrowStatement;
    }
};

class ForLoop : public ASTNode
{
public:
    std::vector<Parameter> parameters;
    std::unique_ptr<ASTNode> iterable;
    std::unique_ptr<ASTNode> body;

    ForLoop(std::vector<Parameter> parameters, std::unique_ptr<ASTNode> iterable, std::unique_ptr<ASTNode> body)
        : parameters(std::move(parameters)), iterable(std::move(iterable)), body(std::move(body))
    {
        if (body && body->getType() != NodeType::Block)
        {
            throw RuntimeError("'For/ForEach' body must be of type <Block>", -1, -1);
        }
    }

    std::string toString() const override
    {
        std::ostringstream oss;
        oss << "ForLoop(parameters = [";

        for (size_t i = 0; i < parameters.size(); ++i)
        {
            const auto &param = parameters[i];
            oss << (param.isOptional ? "optional " : "") << param.name << " oftype " << (param.type.empty() ? "any" : param.type);

            if (param.defaultValue)
            {
                oss << " default " << param.defaultValue->toString();
            }

            if (i < parameters.size() - 1)
            {
                oss << ", ";
            }
        }

        oss << "], iterable = " << iterable->toString()
            << ", body = " << (body ? body->toString() : "null") << ")";
        return oss.str();
    }

    NodeType getType() const override
    {
        return NodeType::ForLoop;
    }
};

class WhileLoop : public ASTNode
{
public:
    std::unique_ptr<ASTNode> condition;
    std::unique_ptr<ASTNode> body;

    WhileLoop(std::unique_ptr<ASTNode> condition, std::unique_ptr<ASTNode> body)
        : condition(std::move(condition)), body(std::move(body))
    {
        if (body && body->getType() != NodeType::Block)
        {
            throw RuntimeError("'While/AsLongAs' body must be of type <Block>", -1, -1);
        }
    }

    std::string toString() const override
    {
        std::ostringstream oss;
        oss << "WhileLoop(condition = " << condition->toString()
            << ", body = " << (body ? body->toString() : "null") << ")";
        return oss.str();
    }

    NodeType getType() const override
    {
        return NodeType::WhileLoop;
    }
};

#endif // FIDAN_AST_H