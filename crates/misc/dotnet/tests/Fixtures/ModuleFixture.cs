using System;
using System.IO;
using Wasmtime;

namespace Wasmtime.Tests
{
    public abstract class ModuleFixture : IDisposable
    {
        public ModuleFixture()
        {
            Engine = new Engine();
            Store = Engine.CreateStore();
            Module = Store.CreateModule(Path.Combine("Modules", ModuleFileName));
        }

        public void Dispose()
        {
            if (Module != null)
            {
                Module.Dispose();
                Module = null;
            }

            if (Store != null)
            {
                Store.Dispose();
                Store = null;
            }

            if (Engine != null)
            {
                Engine.Dispose();
                Engine = null;
            }
        }

        public Engine Engine { get; set; }
        public Store Store { get; set; }
        public Module Module { get; set; }

        protected abstract string ModuleFileName { get; }
    }
}
