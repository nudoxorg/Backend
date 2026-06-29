# Intermediate Representation

The universal, closed-system representation for all of the relevant code in our system.

## Goals
- To represent the contracts of an inputted package(repository, usually)
- Be version agnostic, not providing any specialization for a particular language, and yet cover many classes of languages (ML, Functional, etc.)
- To provide the contract at the fullest resolution, including the absurdity of certain systems found in Haskell and Typescript.

## Non-goals
- To provide the means to represent the code in it's original form/syntax
