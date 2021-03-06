﻿using System;
using System.Collections.Generic;
using System.Linq;
using System.Runtime.CompilerServices;
using System.Runtime.InteropServices.WindowsRuntime;
using System.Text;
using System.Threading.Tasks;

namespace Se
{
    public abstract class Component
    {
        public Entity Entity { get; set; }
    }

    public class TagComponent : Component
    {
        public string Tag
        {
            get => GetTag_Native(Entity.ID);
            set => SetTag_Native(value);
        }

        [MethodImpl(MethodImplOptions.InternalCall)]
        private static extern string GetTag_Native(ulong entityID);

        [MethodImpl(MethodImplOptions.InternalCall)]
        private static extern void SetTag_Native(string tag);

    }

    public class TransformComponent : Component
    {
        public Matrix4 Transform
        {
            get
            {
                GetTransform_Native(Entity.ID, out var result);
                return result;
            }
            set => SetTransform_Native(Entity.ID, ref value);
        }

        [MethodImpl(MethodImplOptions.InternalCall)]
        private static extern void GetTransform_Native(ulong entityID, out Matrix4 result);

        [MethodImpl(MethodImplOptions.InternalCall)]
        private static extern void SetTransform_Native(ulong entityID, ref Matrix4 result);

    }

    public class MeshComponent : Component
    {
        public Mesh Mesh
        {
            get
            {
                var result = new Mesh(GetMesh_Native(Entity.ID));
                return result;
            }
            set
            {
                var ptr = value == null ? IntPtr.Zero : value.m_UnmanagedInstance;
                SetMesh_Native(Entity.ID, ptr);
            }
        }

        [MethodImpl(MethodImplOptions.InternalCall)]
        private static extern IntPtr GetMesh_Native(ulong entityID);

        [MethodImpl(MethodImplOptions.InternalCall)]
        private static extern void SetMesh_Native(ulong entityID, IntPtr unmanagedInstance);

    }

    public class CameraComponent : Component
    {
        public Camera Camera
        {
            get
            {
                var result = new Camera(GetCamera_Native(Entity.ID));
                return result;
            }
            set
            {
                IntPtr ptr = value == null ? IntPtr.Zero : value.m_UnmanagedInstance;
                SetCamera_Native(Entity.ID, ptr);
            }
        }


        [MethodImpl(MethodImplOptions.InternalCall)]
        private static extern IntPtr GetCamera_Native(ulong entityID);

        [MethodImpl(MethodImplOptions.InternalCall)]
        private static extern void SetCamera_Native(ulong entityID, IntPtr unmanagedInstance);
    }

    public class ScriptComponent : Component
    {
        public String ModuleName
        {
            get => GetModuleName_Native(Entity.ID);
            set => SetModuleName_Native(Entity.ID, value);
        }


        [MethodImpl(MethodImplOptions.InternalCall)]
        internal static extern String GetModuleName_Native(ulong entityID);

        [MethodImpl(MethodImplOptions.InternalCall)]
        internal static extern void SetModuleName_Native(ulong entityID, String moduleName);
    }

    public class SpriteRendererComponent : Component
    {
        // TODO
    }

    public class RigidBody2DComponent : Component
    {
        public void ApplyLinearImpulse(Vector2 impulse, Vector2 offset, bool wake = true)
        {
            ApplyLinearImpulse_Native(Entity.ID, ref impulse, ref offset, wake);
        }

        public Vector2 LinearVelocity
        {
            get
            {
                GetLinearVelocity_Native(Entity.ID, out Vector2 velocity);
                return velocity;
            }
            set => SetLinearVelocity_Native(Entity.ID, ref value);
        }


        [MethodImpl(MethodImplOptions.InternalCall)]
        private static extern void ApplyLinearImpulse_Native(ulong entityID, ref Vector2 impulse, ref Vector2 offset, bool wake);

        [MethodImpl(MethodImplOptions.InternalCall)]
        private static extern void GetLinearVelocity_Native(ulong entityID, out Vector2 velocity);
        [MethodImpl(MethodImplOptions.InternalCall)]
        private static extern void SetLinearVelocity_Native(ulong entityID, ref Vector2 velocity);
    }

    public class Collider2DComponent : Component
    {
        public Vector2 Offset
        {
            get
            {
                GetOffset_Native(Entity.ID, out Vector2 offset);
                return offset;
            }
            set => SetOffset_Native(Entity.ID, ref value);
        }
        public float Density
        {
            get => GetDensity_Native(Entity.ID);
            set => SetDensity_Native(Entity.ID, value);
        }

        public float Friction
        {
            get => GetDensity_Native(Entity.ID);
            set => SetDensity_Native(Entity.ID, value);
        }


        [MethodImpl(MethodImplOptions.InternalCall)]
        private static extern float GetOffset_Native(ulong entityID, out Vector2 offset);
        [MethodImpl(MethodImplOptions.InternalCall)]
        private static extern void SetOffset_Native(ulong entityID, ref Vector2 offset);

        [MethodImpl(MethodImplOptions.InternalCall)]
        private static extern float GetDensity_Native(ulong entityID);
        [MethodImpl(MethodImplOptions.InternalCall)]
        private static extern void SetDensity_Native(ulong entityID, float density);

        [MethodImpl(MethodImplOptions.InternalCall)]
        internal static extern float GetFriction_Native(ulong entityID);
        [MethodImpl(MethodImplOptions.InternalCall)]
        internal static extern void SetFriction_Native(ulong entityID, float friction);

    }

    public class BoxCollider2DComponent : Collider2DComponent
    {
        public Vector2 Size
        {
            get
            {
                GetSize_Native(Entity.ID, out Vector2 size);
                return size;
            }
            set => SetSize_Native(Entity.ID, ref value);
        }


        [MethodImpl(MethodImplOptions.InternalCall)]
        private static extern float GetSize_Native(ulong entityID, out Vector2 size);
        [MethodImpl(MethodImplOptions.InternalCall)]
        private static extern void SetSize_Native(ulong entityID, ref Vector2 size);
    }
    public class CircleCollider2DComponent : Collider2DComponent
    {
        public float Radius
        {
            get => GetRadius_Native(Entity.ID);
            set => SetRadius_Native(Entity.ID, value);
        }


        [MethodImpl(MethodImplOptions.InternalCall)]
        private static extern float GetRadius_Native(ulong entityID);
        [MethodImpl(MethodImplOptions.InternalCall)]
        private static extern void SetRadius_Native(ulong entityID, float radius);
    }

    public class RigidBody3DComponent : Component
    {
        public void ApplyLinearImpulse(Vector3 impulse, Vector3 offset, bool wake = true)
        {
            ApplyLinearImpulse_Native(Entity.ID, ref impulse, ref offset, wake);
        }

        public Vector3 LinearVelocity
        {
            get
            {
                GetLinearVelocity_Native(Entity.ID, out Vector3 velocity);
                return velocity;
            }
            set => SetLinearVelocity_Native(Entity.ID, ref value);
        }


        [MethodImpl(MethodImplOptions.InternalCall)]
        private static extern void ApplyLinearImpulse_Native(ulong entityID, ref Vector3 impulse, ref Vector3 offset, bool wake);

        [MethodImpl(MethodImplOptions.InternalCall)]
        private static extern void GetLinearVelocity_Native(ulong entityID, out Vector3 velocity);
        [MethodImpl(MethodImplOptions.InternalCall)]
        private static extern void SetLinearVelocity_Native(ulong entityID, ref Vector3 velocity);
    }

    public class Collider3DComponent : Component
    {
        public Vector3 Offset
        {
            get
            {
                GetOffset_Native(Entity.ID, out Vector3 offset);
                return offset;
            }
            set => SetOffset_Native(Entity.ID, ref value);
        }
        public float Density
        {
            get => GetDensity_Native(Entity.ID);
            set => SetDensity_Native(Entity.ID, value);
        }

        public float Friction
        {
            get => GetDensity_Native(Entity.ID);
            set => SetDensity_Native(Entity.ID, value);
        }


        [MethodImpl(MethodImplOptions.InternalCall)]
        private static extern float GetOffset_Native(ulong entityID, out Vector3 offset);
        [MethodImpl(MethodImplOptions.InternalCall)]
        private static extern void SetOffset_Native(ulong entityID, ref Vector3 offset);

        [MethodImpl(MethodImplOptions.InternalCall)]
        private static extern float GetDensity_Native(ulong entityID);
        [MethodImpl(MethodImplOptions.InternalCall)]
        private static extern void SetDensity_Native(ulong entityID, float density);

        [MethodImpl(MethodImplOptions.InternalCall)]
        internal static extern float GetFriction_Native(ulong entityID);
        [MethodImpl(MethodImplOptions.InternalCall)]
        internal static extern void SetFriction_Native(ulong entityID, float friction);

    }

    public class BoxCollider3DComponent : Collider3DComponent
    {
        public Vector3 Size
        {
            get
            {
                GetSize_Native(Entity.ID, out Vector3 size);
                return size;
            }
            set => SetSize_Native(Entity.ID, ref value);
        }


        [MethodImpl(MethodImplOptions.InternalCall)]
        private static extern float GetSize_Native(ulong entityID, out Vector3 size);
        [MethodImpl(MethodImplOptions.InternalCall)]
        private static extern void SetSize_Native(ulong entityID, ref Vector3 size);
    }
    public class SphereCollider3DComponent : Collider3DComponent
    {
        public float Radius
        {
            get => GetRadius_Native(Entity.ID);
            set => SetRadius_Native(Entity.ID, value);
        }


        [MethodImpl(MethodImplOptions.InternalCall)]
        private static extern float GetRadius_Native(ulong entityID);
        [MethodImpl(MethodImplOptions.InternalCall)]
        private static extern void SetRadius_Native(ulong entityID, float radius);
    }

}
