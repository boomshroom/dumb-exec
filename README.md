# dumb-exec

A barebones executor for futures that can run in a zero-allocation environment.
The current implementation runs futures to completion the moment they're run.
This behavior, while in the theme of the name, is likely to change.
There are also plans to provide an executor that depends solely on the `alloc` crate,
As well as versions to use in multithreaded contexts such as multiproccessing operating systems.